use std::sync::Arc;

use ai_agent_coordinator::{
    config::WorkerConfig,
    db::Database,
    jobs::{
        ClaimJobRequest, CompleteJobRequest, CompletionOutcome, CreateJobRequest, Job, JobStatus,
    },
    lease::{LeaseError, ManualClock},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

const LEASE_SECONDS: i64 = 60;

/// The claim-side expiry sweep is global across the jobs table, so a test that
/// advances its clock past a lease boundary would expire leases held by tests
/// running concurrently in this binary. Every database test holds this lock.
static DATABASE_TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Opens the test database with a deterministic clock pinned to a fixed
/// instant, so lease boundaries are exercised without sleeping.
async fn clocked_database() -> Option<(Database, ManualClock, MutexGuard<'static, ()>)> {
    let (database, guard) = test_database().await?;
    let start = DateTime::parse_from_rfc3339("2026-09-12T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let clock = ManualClock::new(start);
    Some((database.with_clock(Arc::new(clock.clone())), clock, guard))
}

async fn enqueue_and_claim(
    database: &Database,
    worker_id: &str,
    max_attempts: i64,
) -> (Job, String) {
    let org = format!("lease-{}", Uuid::new_v4());
    database
        .create_job(
            &CreateJobRequest {
                org: org.clone(),
                repo: "coordinator".to_owned(),
                task_type: "code_change".to_owned(),
                payload: json!({"ticket": "DEN-1873"}),
                priority: 0,
                max_attempts,
                available_at: None,
                budget_usd: None,
            },
            None,
        )
        .await
        .unwrap();
    let job = claim(database, &org, worker_id).await.expect("job claimed");
    (job, org)
}

async fn claim(database: &Database, org: &str, worker_id: &str) -> Option<Job> {
    database
        .claim_job(
            &ClaimJobRequest {
                worker_id: worker_id.to_owned(),
                orgs: vec![org.to_owned()],
                repositories: vec![],
                task_types: vec![],
                lease_seconds: LEASE_SECONDS,
            },
            &WorkerConfig::default(),
        )
        .await
        .unwrap()
}

fn success(worker_id: &str) -> CompleteJobRequest {
    CompleteJobRequest {
        worker_id: worker_id.to_owned(),
        outcome: CompletionOutcome::Succeeded,
        result: Some(json!({"pr": 1873})),
        error: None,
        retryable: false,
        retry_delay_seconds: 0,
    }
}

fn lease_error(error: anyhow::Error) -> LeaseError {
    *error
        .downcast_ref::<LeaseError>()
        .unwrap_or_else(|| panic!("expected LeaseError, got {error:#}"))
}

async fn test_database() -> Option<(Database, MutexGuard<'static, ()>)> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
        return None;
    };
    let guard = DATABASE_TEST_LOCK.lock().await;
    Some((
        Database::open(&url)
            .await
            .expect("connect to test database"),
        guard,
    ))
}

#[tokio::test]
async fn job_lifecycle_is_leased_and_idempotent() {
    let Some((database, _guard)) = test_database().await else {
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
    let Some((database, _guard)) = test_database().await else {
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
async fn heartbeat_before_expiry_renews_and_is_idempotent_for_the_holder() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, _) = enqueue_and_claim(&database, "worker-1", 3).await;
    let first_expiry = job.lease_expires_at.unwrap();

    clock.set(first_expiry - Duration::microseconds(1_000));
    let renewed = database
        .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
        .await
        .unwrap();
    let renewed_expiry = renewed.lease_expires_at.unwrap();
    assert!(renewed_expiry > first_expiry);
    assert_eq!(renewed.claimed_by.as_deref(), Some("worker-1"));

    // A retried heartbeat at the same instant yields the same lease.
    let retried = database
        .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
        .await
        .unwrap();
    assert_eq!(retried.lease_expires_at, Some(renewed_expiry));
    assert_eq!(retried.status, JobStatus::Running);
    assert_eq!(retried.attempts, 1);
}

#[tokio::test]
async fn heartbeat_at_or_after_expiry_is_rejected_and_cannot_revive_ownership() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, org) = enqueue_and_claim(&database, "worker-1", 3).await;
    let expiry = job.lease_expires_at.unwrap();

    for instant in [expiry, expiry + Duration::seconds(30)] {
        clock.set(instant);
        let error = database
            .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
            .await
            .unwrap_err();
        assert_eq!(lease_error(error), LeaseError::Expired);
        let unchanged = database.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(unchanged.lease_expires_at, Some(expiry));
    }

    // The expired lease is reassignable at exactly the same boundary.
    clock.set(expiry);
    let reassigned = claim(&database, &org, "worker-2").await.unwrap();
    assert_eq!(reassigned.id, job.id);
    assert_eq!(reassigned.claimed_by.as_deref(), Some("worker-2"));
}

#[tokio::test]
async fn completion_just_before_expiry_succeeds() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, _) = enqueue_and_claim(&database, "worker-1", 3).await;
    clock.set(job.lease_expires_at.unwrap() - Duration::microseconds(1_000));

    let completed = database
        .complete_job(&job.id, &success("worker-1"))
        .await
        .unwrap();
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(completed.claimed_by, None);
}

#[tokio::test]
async fn completion_at_or_after_expiry_is_rejected_without_mutation() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, _) = enqueue_and_claim(&database, "worker-1", 3).await;
    let expiry = job.lease_expires_at.unwrap();

    for instant in [expiry, expiry + Duration::seconds(1)] {
        clock.set(instant);
        for request in [
            success("worker-1"),
            CompleteJobRequest {
                outcome: CompletionOutcome::Failed,
                result: None,
                error: Some("late failure".to_owned()),
                retryable: true,
                ..success("worker-1")
            },
        ] {
            let error = database.complete_job(&job.id, &request).await.unwrap_err();
            assert_eq!(lease_error(error), LeaseError::Expired);
        }
    }

    let unchanged = database.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(unchanged.status, JobStatus::Running);
    assert_eq!(unchanged.claimed_by.as_deref(), Some("worker-1"));
    assert_eq!(unchanged.result, None);
    assert_eq!(unchanged.last_error, None);
}

#[tokio::test]
async fn stale_holder_cannot_heartbeat_or_complete_after_reassignment() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, org) = enqueue_and_claim(&database, "worker-1", 3).await;

    clock.set(job.lease_expires_at.unwrap() + Duration::seconds(5));
    let reassigned = claim(&database, &org, "worker-2").await.unwrap();
    assert_eq!(reassigned.id, job.id);
    assert_eq!(reassigned.attempts, 2);

    // The new lease is live, but worker-1 no longer holds it.
    let error = database
        .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
        .await
        .unwrap_err();
    assert_eq!(lease_error(error), LeaseError::HeldByAnotherWorker);
    let error = database
        .complete_job(&job.id, &success("worker-1"))
        .await
        .unwrap_err();
    assert_eq!(lease_error(error), LeaseError::HeldByAnotherWorker);

    let still_leased = database.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(still_leased.claimed_by.as_deref(), Some("worker-2"));
    assert_eq!(still_leased.lease_expires_at, reassigned.lease_expires_at);

    let completed = database
        .complete_job(&job.id, &success("worker-2"))
        .await
        .unwrap();
    assert_eq!(completed.status, JobStatus::Succeeded);

    // A late replay from either worker cannot rewrite the terminal row.
    for worker in ["worker-1", "worker-2"] {
        let error = database
            .complete_job(&job.id, &success(worker))
            .await
            .unwrap_err();
        assert_eq!(
            lease_error(error),
            LeaseError::NotRunning(JobStatus::Succeeded)
        );
    }
}

#[tokio::test]
async fn cancellation_stays_terminal_against_late_heartbeat_and_completion() {
    let Some((database, _clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, _) = enqueue_and_claim(&database, "worker-1", 3).await;
    database.cancel_job(&job.id).await.unwrap();

    let error = database
        .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
        .await
        .unwrap_err();
    assert_eq!(
        lease_error(error),
        LeaseError::NotRunning(JobStatus::Cancelled)
    );
    let error = database
        .complete_job(&job.id, &success("worker-1"))
        .await
        .unwrap_err();
    assert_eq!(
        lease_error(error),
        LeaseError::NotRunning(JobStatus::Cancelled)
    );
    let cancelled = database.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
}

#[tokio::test]
async fn final_attempt_expiry_cannot_be_revived() {
    let Some((database, clock, _guard)) = clocked_database().await else {
        return;
    };
    let (job, org) = enqueue_and_claim(&database, "worker-1", 1).await;
    clock.set(job.lease_expires_at.unwrap());

    let error = database
        .heartbeat_job(&job.id, "worker-1", LEASE_SECONDS)
        .await
        .unwrap_err();
    assert_eq!(lease_error(error), LeaseError::Expired);

    // The claim-side sweep fails the exhausted job at the same boundary.
    assert!(claim(&database, &org, "worker-2").await.is_none());
    let failed = database.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(failed.status, JobStatus::Failed);

    let error = database
        .complete_job(&job.id, &success("worker-1"))
        .await
        .unwrap_err();
    assert_eq!(
        lease_error(error),
        LeaseError::NotRunning(JobStatus::Failed)
    );
}

#[tokio::test]
async fn missing_job_is_reported_as_not_found() {
    let Some((database, _clock, _guard)) = clocked_database().await else {
        return;
    };
    let missing = Uuid::new_v4().to_string();
    let error = database
        .heartbeat_job(&missing, "worker-1", LEASE_SECONDS)
        .await
        .unwrap_err();
    assert_eq!(lease_error(error), LeaseError::NotFound);
    let error = database
        .complete_job(&missing, &success("worker-1"))
        .await
        .unwrap_err();
    assert_eq!(lease_error(error), LeaseError::NotFound);
}
