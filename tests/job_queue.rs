use ai_agent_coordinator::{
    config::WorkerConfig,
    db::Database,
    jobs::{ClaimJobRequest, CompleteJobRequest, CompletionOutcome, CreateJobRequest, JobStatus},
    worker_authority::{ClaimTaskPolicy, LINEAR_OPINION_CHATGPT},
};
use sea_orm::{ConnectionTrait, Database as SeaOrmDatabase, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

async fn test_database() -> Option<Database> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
        return None;
    };
    Some(
        Database::open(&url)
            .await
            .expect("connect to test database"),
    )
}

async fn expire_job_lease(job_id: &str) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL remains configured");
    let connection = SeaOrmDatabase::connect(&url)
        .await
        .expect("connect for lease-expiry fixture");
    connection
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE ai_agent_coordinator.jobs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
            [job_id.to_owned().into()],
        ))
        .await
        .expect("expire the claimed lease");
}

#[tokio::test]
async fn job_lifecycle_is_leased_and_idempotent() {
    let Some(database) = test_database().await else {
        return;
    };
    let org = format!("job-lifecycle-{}", Uuid::new_v4());
    let idempotency_key = format!("linear:{}", Uuid::new_v4());
    let request = CreateJobRequest {
        org: org.clone(),
        repo: "coordinator".to_owned(),
        task_type: "code_change".to_owned(),
        payload: json!({"ticket": "ENG-1"}),
        priority: 10,
        max_attempts: 3,
        available_at: None,
        budget_usd: Some(1.0),
    };

    let first = database
        .create_job(&request, Some(&idempotency_key))
        .await
        .unwrap();
    let duplicate = database
        .create_job(&request, Some(&idempotency_key))
        .await
        .unwrap();
    assert_eq!(first.id, duplicate.id);

    let claimed = database
        .claim_job(
            &ClaimJobRequest {
                worker_id: "worker-1".to_owned(),
                orgs: vec![org],
                repositories: vec![],
                task_types: vec![],
                lease_seconds: 60,
            },
            &WorkerConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.status, JobStatus::Running);
    assert_eq!(claimed.attempts, 1);

    let completed = database
        .complete_job(
            &claimed.id,
            &CompleteJobRequest {
                worker_id: "worker-1".to_owned(),
                lease_attempt: Some(claimed.attempts),
                outcome: CompletionOutcome::Succeeded,
                result: Some(json!({"pr": 42})),
                error: None,
                retryable: false,
                retry_delay_seconds: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(completed.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn repository_concurrency_cap_prevents_overclaiming() {
    let Some(database) = test_database().await else {
        return;
    };
    let org = format!("repo-cap-{}", Uuid::new_v4());
    for ticket in ["ENG-2", "ENG-3"] {
        database
            .create_job(
                &CreateJobRequest {
                    org: org.clone(),
                    repo: "busy-repo".to_owned(),
                    task_type: "code_change".to_owned(),
                    payload: json!({"ticket": ticket}),
                    priority: 0,
                    max_attempts: 3,
                    available_at: None,
                    budget_usd: None,
                },
                Some(&format!("{ticket}:{}", Uuid::new_v4())),
            )
            .await
            .unwrap();
    }

    let worker_config = WorkerConfig {
        default_org_concurrency: 10,
        default_repo_concurrency: 1,
        org_concurrency: Default::default(),
        repo_concurrency: Default::default(),
    };
    let claim = |worker_id: &str| ClaimJobRequest {
        worker_id: worker_id.to_owned(),
        orgs: vec![org.clone()],
        repositories: vec!["busy-repo".to_owned()],
        task_types: vec![],
        lease_seconds: 60,
    };

    let first_request = claim("worker-1");
    let second_request = claim("worker-2");
    let (first, second) = tokio::join!(
        database.claim_job(&first_request, &worker_config),
        database.claim_job(&second_request, &worker_config),
    );
    let claimed = [first.unwrap(), second.unwrap()]
        .into_iter()
        .filter(Option::is_some)
        .count();
    assert_eq!(claimed, 1);
}

#[tokio::test]
async fn broad_workers_cannot_claim_protected_tasks() {
    let Some(database) = test_database().await else {
        return;
    };
    let org = format!("protected-claim-{}", Uuid::new_v4());
    for (task_type, priority) in [(LINEAR_OPINION_CHATGPT, 100), ("code_change", 0)] {
        database
            .create_job(
                &CreateJobRequest {
                    org: org.clone(),
                    repo: "coordinator".to_owned(),
                    task_type: task_type.to_owned(),
                    payload: json!({"test": "protected-claim-boundary"}),
                    priority,
                    max_attempts: 3,
                    available_at: None,
                    budget_usd: None,
                },
                Some(&format!("{task_type}:{}", Uuid::new_v4())),
            )
            .await
            .unwrap();
    }

    let broad = ClaimJobRequest {
        worker_id: "generic-worker".to_owned(),
        orgs: vec![org.clone()],
        repositories: vec![],
        task_types: vec![],
        lease_seconds: 60,
    };
    let first = database
        .claim_job(&broad, &WorkerConfig::default())
        .await
        .unwrap()
        .expect("generic worker should receive the unprotected job");
    assert_eq!(first.task_type, "code_change");
    assert!(database
        .claim_job(&broad, &WorkerConfig::default())
        .await
        .unwrap()
        .is_none());

    let protected = ClaimJobRequest {
        worker_id: "linear-opinion-openai".to_owned(),
        orgs: vec![org],
        repositories: vec![],
        task_types: vec![LINEAR_OPINION_CHATGPT.to_owned()],
        lease_seconds: 60,
    };
    assert!(database
        .claim_job(&protected, &WorkerConfig::default())
        .await
        .unwrap()
        .is_none());

    let policy = ClaimTaskPolicy::Only([LINEAR_OPINION_CHATGPT.to_owned()].into_iter().collect());
    let claimed = database
        .claim_job_authorized(&protected, &WorkerConfig::default(), &policy)
        .await
        .unwrap()
        .expect("the exact role policy should receive its protected task");
    assert_eq!(claimed.task_type, LINEAR_OPINION_CHATGPT);
    assert_eq!(claimed.claimed_by.as_deref(), Some("linear-opinion-openai"));
}

#[tokio::test]
async fn expired_and_reacquired_leases_reject_stale_mutations() {
    let Some(database) = test_database().await else {
        return;
    };
    let org = format!("lease-fence-{}", Uuid::new_v4());
    let created = database
        .create_job(
            &CreateJobRequest {
                org: org.clone(),
                repo: "coordinator".to_owned(),
                task_type: LINEAR_OPINION_CHATGPT.to_owned(),
                payload: json!({"test": "lease-fence"}),
                priority: 0,
                max_attempts: 3,
                available_at: None,
                budget_usd: None,
            },
            Some(&format!("lease-fence:{}", Uuid::new_v4())),
        )
        .await
        .unwrap();
    let request = ClaimJobRequest {
        worker_id: "linear-opinion-openai".to_owned(),
        orgs: vec![org],
        repositories: vec![],
        task_types: vec![LINEAR_OPINION_CHATGPT.to_owned()],
        lease_seconds: 60,
    };
    let policy = ClaimTaskPolicy::Only([LINEAR_OPINION_CHATGPT.to_owned()].into_iter().collect());
    let first = database
        .claim_job_authorized(&request, &WorkerConfig::default(), &policy)
        .await
        .unwrap()
        .expect("first protected lease");
    assert_eq!(first.id, created.id);
    assert_eq!(first.attempts, 1);

    expire_job_lease(&first.id).await;
    assert!(database
        .heartbeat_job(&first.id, "linear-opinion-openai", Some(first.attempts), 60)
        .await
        .is_err());
    assert!(database
        .complete_job(
            &first.id,
            &CompleteJobRequest {
                worker_id: "linear-opinion-openai".to_owned(),
                lease_attempt: Some(first.attempts),
                outcome: CompletionOutcome::Succeeded,
                result: None,
                error: None,
                retryable: false,
                retry_delay_seconds: 0,
            },
        )
        .await
        .is_err());

    let second = database
        .claim_job_authorized(&request, &WorkerConfig::default(), &policy)
        .await
        .unwrap()
        .expect("expired lease is reclaimed");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempts, 2);

    assert!(database
        .heartbeat_job(
            &second.id,
            "linear-opinion-openai",
            Some(first.attempts),
            60
        )
        .await
        .is_err());
    assert!(database
        .complete_job(
            &second.id,
            &CompleteJobRequest {
                worker_id: "linear-opinion-openai".to_owned(),
                lease_attempt: Some(first.attempts),
                outcome: CompletionOutcome::Succeeded,
                result: None,
                error: None,
                retryable: false,
                retry_delay_seconds: 0,
            },
        )
        .await
        .is_err());

    let heartbeat = database
        .heartbeat_job(
            &second.id,
            "linear-opinion-openai",
            Some(second.attempts),
            60,
        )
        .await
        .expect("current lease fence heartbeats");
    assert_eq!(heartbeat.attempts, second.attempts);
    let completed = database
        .complete_job(
            &second.id,
            &CompleteJobRequest {
                worker_id: "linear-opinion-openai".to_owned(),
                lease_attempt: Some(second.attempts),
                outcome: CompletionOutcome::Succeeded,
                result: Some(json!({"lease_fence": second.attempts})),
                error: None,
                retryable: false,
                retry_delay_seconds: 0,
            },
        )
        .await
        .expect("current lease fence completes");
    assert_eq!(completed.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn protected_backlog_cannot_starve_an_unprotected_claim_window() {
    let Some(database) = test_database().await else {
        return;
    };
    let org = format!("claim-window-{}", Uuid::new_v4());
    for index in 0..201 {
        database
            .create_job(
                &CreateJobRequest {
                    org: org.clone(),
                    repo: "coordinator".to_owned(),
                    task_type: LINEAR_OPINION_CHATGPT.to_owned(),
                    payload: json!({"index": index}),
                    priority: 100,
                    max_attempts: 3,
                    available_at: None,
                    budget_usd: None,
                },
                Some(&format!("protected-window:{index}:{}", Uuid::new_v4())),
            )
            .await
            .unwrap();
    }
    let ordinary = database
        .create_job(
            &CreateJobRequest {
                org: org.clone(),
                repo: "coordinator".to_owned(),
                task_type: "code_change".to_owned(),
                payload: json!({"test": "claim-window"}),
                priority: -100,
                max_attempts: 3,
                available_at: None,
                budget_usd: None,
            },
            Some(&format!("ordinary-window:{}", Uuid::new_v4())),
        )
        .await
        .unwrap();

    let claimed = database
        .claim_job(
            &ClaimJobRequest {
                worker_id: "generic-worker".to_owned(),
                orgs: vec![org],
                repositories: vec![],
                task_types: vec![],
                lease_seconds: 60,
            },
            &WorkerConfig::default(),
        )
        .await
        .unwrap()
        .expect("policy filtering occurs before the bounded candidate window");
    assert_eq!(claimed.id, ordinary.id);
    assert_eq!(claimed.task_type, "code_change");
}
