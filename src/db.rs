//! SeaORM persistence for the coordinator's PostgreSQL namespace.
//!
//! The application never creates or migrates tables. The declarative schema
//! authority lives in k8s-libs-and-shared-defs at:
//! `pg-defs/schema/databases/ai_agent_coordinator/schema.sql`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sea_orm::{
    sea_query::{Expr, LockBehavior, LockType, OnConflict},
    AccessMode, ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectOptions, Database as SeaDatabase, DatabaseConnection, DatabaseTransaction,
    EntityTrait, IsolationLevel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use uuid::Uuid;

use crate::{
    config::WorkerConfig,
    entity::{jobs, linear_mutations, model_usage},
    jobs::{
        ClaimJobRequest, CompleteJobRequest, CompletionOutcome, CreateJobRequest, Job, JobStatus,
    },
    worker_authority::ClaimTaskPolicy,
};

const SERIALIZABLE_RETRIES: usize = 4;

#[derive(Clone)]
pub struct Database {
    connection: DatabaseConnection,
}

#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub request_id: String,
    pub org: String,
    pub repo: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

impl Database {
    pub async fn open(database_url: &str) -> Result<Self> {
        if !matches!(
            database_url.split_once("://").map(|(scheme, _)| scheme),
            Some("postgres" | "postgresql")
        ) {
            return Err(anyhow!(
                "database URL must use the postgres:// or postgresql:// scheme"
            ));
        }

        let mut options = ConnectOptions::new(database_url.to_owned());
        options
            .max_connections(16)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(300))
            .sqlx_logging(true);
        let connection = SeaDatabase::connect(options)
            .await
            .context("failed to connect to the coordinator PostgreSQL database")?;
        Ok(Self { connection })
    }

    pub async fn ready(&self) -> Result<()> {
        self.connection
            .ping()
            .await
            .context("database readiness check failed")
    }

    pub async fn create_job(
        &self,
        request: &CreateJobRequest,
        idempotency_key: Option<&str>,
    ) -> Result<Job> {
        request.validate().map_err(anyhow::Error::msg)?;
        request
            .validate_idempotency_key(idempotency_key)
            .map_err(anyhow::Error::msg)?;

        if let Some(key) = idempotency_key {
            if let Some(existing) = jobs::Entity::find()
                .filter(jobs::Column::IdempotencyKey.eq(key))
                .one(&self.connection)
                .await?
            {
                return model_to_job(existing);
            }
        }

        let now = Utc::now();
        let id = Uuid::new_v4().to_string();
        let active_model = jobs::ActiveModel {
            id: Set(id.clone()),
            org: Set(request.org.clone()),
            repo: Set(request.repo.clone()),
            task_type: Set(request.task_type.clone()),
            payload: Set(request.payload.clone()),
            priority: Set(request.priority),
            status: Set(JobStatus::Queued.as_str().to_owned()),
            idempotency_key: Set(idempotency_key.map(str::to_owned)),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            available_at: Set(request.available_at.unwrap_or(now).into()),
            claimed_by: Set(None),
            lease_expires_at: Set(None),
            attempts: Set(0),
            max_attempts: Set(request.max_attempts),
            result: Set(None),
            last_error: Set(None),
            budget_usd: Set(request.budget_usd),
        };

        if let Some(key) = idempotency_key {
            let inserted = jobs::Entity::insert(active_model)
                .on_conflict(
                    OnConflict::column(jobs::Column::IdempotencyKey)
                        .do_nothing()
                        .to_owned(),
                )
                .do_nothing()
                .exec(&self.connection)
                .await?;
            if matches!(inserted, sea_orm::TryInsertResult::Inserted(_)) {
                return self
                    .get_job(&id)
                    .await?
                    .ok_or_else(|| anyhow!("newly inserted job could not be read"));
            }
            return jobs::Entity::find()
                .filter(jobs::Column::IdempotencyKey.eq(key))
                .one(&self.connection)
                .await?
                .map(model_to_job)
                .transpose()?
                .ok_or_else(|| anyhow!("idempotent job could not be read after insert conflict"));
        } else {
            jobs::Entity::insert(active_model)
                .exec(&self.connection)
                .await?;
            self.get_job(&id)
                .await?
                .ok_or_else(|| anyhow!("newly inserted job could not be read"))
        }
    }

    pub async fn get_job(&self, id: &str) -> Result<Option<Job>> {
        jobs::Entity::find_by_id(id)
            .one(&self.connection)
            .await?
            .map(model_to_job)
            .transpose()
    }

    pub async fn claim_job(
        &self,
        request: &ClaimJobRequest,
        worker_config: &WorkerConfig,
    ) -> Result<Option<Job>> {
        self.claim_job_authorized(request, worker_config, &ClaimTaskPolicy::ExcludeProtected)
            .await
    }

    pub async fn claim_job_authorized(
        &self,
        request: &ClaimJobRequest,
        worker_config: &WorkerConfig,
        policy: &ClaimTaskPolicy,
    ) -> Result<Option<Job>> {
        request.validate().map_err(anyhow::Error::msg)?;

        for attempt in 0..SERIALIZABLE_RETRIES {
            match self.claim_job_once(request, worker_config, policy).await {
                Ok(job) => return Ok(job),
                Err(error)
                    if attempt + 1 < SERIALIZABLE_RETRIES && is_serialization_failure(&error) =>
                {
                    tokio::task::yield_now().await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("claim retry loop always returns")
    }

    async fn claim_job_once(
        &self,
        request: &ClaimJobRequest,
        worker_config: &WorkerConfig,
        policy: &ClaimTaskPolicy,
    ) -> Result<Option<Job>> {
        let transaction = self
            .connection
            .begin_with_config(
                Some(IsolationLevel::Serializable),
                Some(AccessMode::ReadWrite),
            )
            .await?;
        let now = Utc::now();

        jobs::Entity::update_many()
            .col_expr(jobs::Column::Status, Expr::value("queued"))
            .col_expr(jobs::Column::ClaimedBy, Expr::value(Option::<String>::None))
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .col_expr(jobs::Column::UpdatedAt, Expr::value(now))
            .filter(jobs::Column::Status.eq(JobStatus::Running.as_str()))
            .filter(jobs::Column::LeaseExpiresAt.is_not_null())
            .filter(jobs::Column::LeaseExpiresAt.lt(now))
            .filter(Expr::col(jobs::Column::Attempts).lt(Expr::col(jobs::Column::MaxAttempts)))
            .exec(&transaction)
            .await?;

        jobs::Entity::update_many()
            .col_expr(jobs::Column::Status, Expr::value("failed"))
            .col_expr(
                jobs::Column::LastError,
                Expr::col(jobs::Column::LastError)
                    .if_null("worker lease expired after final attempt"),
            )
            .col_expr(jobs::Column::ClaimedBy, Expr::value(Option::<String>::None))
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .col_expr(jobs::Column::UpdatedAt, Expr::value(now))
            .filter(jobs::Column::Status.eq(JobStatus::Running.as_str()))
            .filter(jobs::Column::LeaseExpiresAt.is_not_null())
            .filter(jobs::Column::LeaseExpiresAt.lt(now))
            .filter(Expr::col(jobs::Column::Attempts).gte(Expr::col(jobs::Column::MaxAttempts)))
            .exec(&transaction)
            .await?;

        let candidates = jobs::Entity::find()
            .filter(jobs::Column::Status.eq(JobStatus::Queued.as_str()))
            .filter(jobs::Column::AvailableAt.lte(now))
            .filter(Expr::col(jobs::Column::Attempts).lt(Expr::col(jobs::Column::MaxAttempts)))
            .order_by_desc(jobs::Column::Priority)
            .order_by_asc(jobs::Column::CreatedAt)
            .limit(200)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .all(&transaction)
            .await?;

        for candidate_model in candidates {
            let candidate = model_to_job(candidate_model)?;
            if !policy.allows(&candidate.task_type) || !request.accepts(&candidate) {
                continue;
            }

            let org_running = jobs::Entity::find()
                .filter(jobs::Column::Status.eq(JobStatus::Running.as_str()))
                .filter(jobs::Column::Org.eq(&candidate.org))
                .count(&transaction)
                .await?;
            if org_running >= worker_config.org_limit(&candidate.org) as u64 {
                continue;
            }

            let repo_running = jobs::Entity::find()
                .filter(jobs::Column::Status.eq(JobStatus::Running.as_str()))
                .filter(jobs::Column::Org.eq(&candidate.org))
                .filter(jobs::Column::Repo.eq(&candidate.repo))
                .count(&transaction)
                .await?;
            if repo_running >= worker_config.repo_limit(&candidate.org, &candidate.repo) as u64 {
                continue;
            }

            let lease_expires_at = now + ChronoDuration::seconds(request.lease_seconds);
            let updated = jobs::Entity::update_many()
                .col_expr(jobs::Column::Status, Expr::value("running"))
                .col_expr(
                    jobs::Column::ClaimedBy,
                    Expr::value(Some(request.worker_id.clone())),
                )
                .col_expr(
                    jobs::Column::LeaseExpiresAt,
                    Expr::value(Some(lease_expires_at)),
                )
                .col_expr(
                    jobs::Column::Attempts,
                    Expr::col(jobs::Column::Attempts).add(1),
                )
                .col_expr(jobs::Column::UpdatedAt, Expr::value(now))
                .filter(jobs::Column::Id.eq(&candidate.id))
                .filter(jobs::Column::Status.eq(JobStatus::Queued.as_str()))
                .exec(&transaction)
                .await?;
            if updated.rows_affected == 1 {
                let claimed = get_job_in(&transaction, &candidate.id)
                    .await?
                    .ok_or_else(|| anyhow!("claimed job could not be read"))?;
                transaction.commit().await?;
                return Ok(Some(claimed));
            }
        }

        transaction.commit().await?;
        Ok(None)
    }

    pub async fn heartbeat_job(
        &self,
        id: &str,
        worker_id: &str,
        lease_seconds: i64,
    ) -> Result<Job> {
        if !(15..=3600).contains(&lease_seconds) {
            return Err(anyhow!("lease_seconds must be between 15 and 3600"));
        }
        let now = Utc::now();
        let lease_expires_at = now + ChronoDuration::seconds(lease_seconds);
        let updated = jobs::Entity::update_many()
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Some(lease_expires_at)),
            )
            .col_expr(jobs::Column::UpdatedAt, Expr::value(now))
            .filter(jobs::Column::Id.eq(id))
            .filter(jobs::Column::Status.eq(JobStatus::Running.as_str()))
            .filter(jobs::Column::ClaimedBy.eq(worker_id))
            .exec(&self.connection)
            .await?;
        if updated.rows_affected != 1 {
            return Err(anyhow!(
                "job is not running, does not exist, or is leased by another worker"
            ));
        }
        self.get_job(id)
            .await?
            .ok_or_else(|| anyhow!("updated job could not be read"))
    }

    pub async fn complete_job(&self, id: &str, request: &CompleteJobRequest) -> Result<Job> {
        let transaction = self.connection.begin().await?;
        let model = jobs::Entity::find_by_id(id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| anyhow!("job not found"))?;
        let job = model_to_job(model.clone())?;

        if job.status != JobStatus::Running {
            return Err(anyhow!("job is not running"));
        }
        if job.claimed_by.as_deref() != Some(request.worker_id.as_str()) {
            return Err(anyhow!("job is leased by another worker"));
        }

        let now = Utc::now();
        let mut active: jobs::ActiveModel = model.into();
        active.result = Set(request.result.clone());
        active.claimed_by = Set(None);
        active.lease_expires_at = Set(None);
        active.updated_at = Set(now.into());

        match request.outcome {
            CompletionOutcome::Succeeded => {
                active.status = Set(JobStatus::Succeeded.as_str().to_owned());
                active.last_error = Set(None);
            }
            CompletionOutcome::Failed if request.retryable && job.attempts < job.max_attempts => {
                active.status = Set(JobStatus::Queued.as_str().to_owned());
                active.last_error = Set(request.error.clone());
                let delay = request.retry_delay_seconds.clamp(0, 86_400);
                active.available_at = Set((now + ChronoDuration::seconds(delay)).into());
            }
            CompletionOutcome::Failed => {
                active.status = Set(JobStatus::Failed.as_str().to_owned());
                active.last_error = Set(request.error.clone());
            }
        }

        let updated = active.update(&transaction).await?;
        transaction.commit().await?;
        model_to_job(updated)
    }

    pub async fn cancel_job(&self, id: &str) -> Result<Job> {
        let now = Utc::now();
        let updated = jobs::Entity::update_many()
            .col_expr(jobs::Column::Status, Expr::value("cancelled"))
            .col_expr(jobs::Column::ClaimedBy, Expr::value(Option::<String>::None))
            .col_expr(
                jobs::Column::LeaseExpiresAt,
                Expr::value(Option::<DateTime<Utc>>::None),
            )
            .col_expr(jobs::Column::UpdatedAt, Expr::value(now))
            .filter(jobs::Column::Id.eq(id))
            .filter(
                jobs::Column::Status
                    .is_in([JobStatus::Queued.as_str(), JobStatus::Running.as_str()]),
            )
            .exec(&self.connection)
            .await?;
        if updated.rows_affected != 1 {
            return Err(anyhow!("job cannot be cancelled or does not exist"));
        }
        self.get_job(id)
            .await?
            .ok_or_else(|| anyhow!("cancelled job could not be read"))
    }

    pub async fn record_usage(&self, record: &UsageRecord) -> Result<()> {
        let prompt_tokens = i64::try_from(record.prompt_tokens)
            .context("prompt token count exceeds PostgreSQL bigint range")?;
        let completion_tokens = i64::try_from(record.completion_tokens)
            .context("completion token count exceeds PostgreSQL bigint range")?;
        model_usage::ActiveModel {
            request_id: Set(record.request_id.clone()),
            created_at: Set(Utc::now().into()),
            org: Set(record.org.clone()),
            repo: Set(record.repo.clone()),
            provider: Set(record.provider.clone()),
            model: Set(record.model.clone()),
            prompt_tokens: Set(prompt_tokens),
            completion_tokens: Set(completion_tokens),
            cost_usd: Set(record.cost_usd),
            ..Default::default()
        }
        .insert(&self.connection)
        .await?;
        Ok(())
    }

    pub async fn org_usage_today_usd(&self, org: &str) -> Result<f64> {
        self.usage_today_usd(org, None).await
    }

    pub async fn repo_usage_today_usd(&self, org: &str, repo: &str) -> Result<f64> {
        self.usage_today_usd(org, Some(repo)).await
    }

    async fn usage_today_usd(&self, org: &str, repo: Option<&str>) -> Result<f64> {
        let mut query = model_usage::Entity::find()
            .select_only()
            .column_as(Expr::col(model_usage::Column::CostUsd).sum(), "total")
            .filter(model_usage::Column::Org.eq(org))
            .filter(model_usage::Column::CreatedAt.gte(start_of_utc_day()));
        if let Some(repo) = repo {
            query = query.filter(model_usage::Column::Repo.eq(repo));
        }
        let total = query
            .into_tuple::<Option<f64>>()
            .one(&self.connection)
            .await?
            .flatten()
            .unwrap_or(0.0);
        Ok(total)
    }

    pub(crate) async fn linear_mutation_succeeded(&self, key: &str) -> Result<bool> {
        let status = linear_mutations::Entity::find_by_id(key)
            .one(&self.connection)
            .await?
            .map(|model| model.status);
        Ok(status.as_deref() == Some("succeeded"))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn begin_linear_mutation(
        &self,
        key: &str,
        job_id: &str,
        organization: &str,
        repository: &str,
        issue_identifier: &str,
        commit_id: &str,
        keyword: &str,
        action: &str,
    ) -> Result<()> {
        let transaction = self.connection.begin().await?;
        let now = Utc::now();
        if let Some(existing) = linear_mutations::Entity::find_by_id(key)
            .lock_exclusive()
            .one(&transaction)
            .await?
        {
            let mut active: linear_mutations::ActiveModel = existing.into();
            active.status = Set("pending".to_owned());
            active.attempts = Set(active.attempts.take().unwrap_or_default() + 1);
            active.last_error = Set(None);
            active.updated_at = Set(now.into());
            active.update(&transaction).await?;
        } else {
            linear_mutations::ActiveModel {
                mutation_key: Set(key.to_owned()),
                job_id: Set(job_id.to_owned()),
                organization: Set(organization.to_owned()),
                repository: Set(repository.to_owned()),
                issue_identifier: Set(issue_identifier.to_owned()),
                commit_id: Set(commit_id.to_owned()),
                keyword: Set(keyword.to_owned()),
                action: Set(action.to_owned()),
                status: Set("pending".to_owned()),
                attempts: Set(1),
                last_error: Set(None),
                created_at: Set(now.into()),
                updated_at: Set(now.into()),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn succeed_linear_mutation(&self, key: &str) -> Result<()> {
        self.finish_linear_mutation(key, "succeeded", None).await
    }

    pub(crate) async fn fail_linear_mutation(&self, key: &str, error: &str) -> Result<()> {
        self.finish_linear_mutation(key, "failed", Some(error))
            .await
    }

    async fn finish_linear_mutation(
        &self,
        key: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let updated = linear_mutations::Entity::update_many()
            .col_expr(linear_mutations::Column::Status, Expr::value(status))
            .col_expr(
                linear_mutations::Column::LastError,
                Expr::value(error.map(|value| value.chars().take(512).collect::<String>())),
            )
            .col_expr(linear_mutations::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(linear_mutations::Column::MutationKey.eq(key))
            .exec(&self.connection)
            .await?;
        if updated.rows_affected != 1 {
            return Err(anyhow!("Linear mutation ledger row does not exist"));
        }
        Ok(())
    }
}

async fn get_job_in(transaction: &DatabaseTransaction, id: &str) -> Result<Option<Job>> {
    jobs::Entity::find_by_id(id)
        .one(transaction)
        .await?
        .map(model_to_job)
        .transpose()
}

fn model_to_job(model: jobs::Model) -> Result<Job> {
    let status = JobStatus::parse(&model.status)
        .ok_or_else(|| anyhow!("database contains invalid job status {:?}", model.status))?;
    Ok(Job {
        id: model.id,
        org: model.org,
        repo: model.repo,
        task_type: model.task_type,
        payload: model.payload,
        priority: model.priority,
        status,
        created_at: model.created_at.with_timezone(&Utc),
        updated_at: model.updated_at.with_timezone(&Utc),
        available_at: model.available_at.with_timezone(&Utc),
        claimed_by: model.claimed_by,
        lease_expires_at: model
            .lease_expires_at
            .map(|timestamp| timestamp.with_timezone(&Utc)),
        attempts: model.attempts,
        max_attempts: model.max_attempts,
        result: model.result,
        last_error: model.last_error,
        budget_usd: model.budget_usd,
    })
}

fn start_of_utc_day() -> DateTime<Utc> {
    Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always valid")
        .and_utc()
}

fn is_serialization_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string().to_ascii_lowercase();
        message.contains("could not serialize access")
            || message.contains("serialization failure")
            || message.contains("sqlstate 40001")
    })
}
