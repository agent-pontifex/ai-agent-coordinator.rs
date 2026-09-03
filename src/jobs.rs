use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub org: String,
    pub repo: String,
    pub task_type: String,
    pub payload: Value,
    pub priority: i64,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub claimed_by: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempts: i64,
    pub max_attempts: i64,
    pub result: Option<Value>,
    pub last_error: Option<String>,
    pub budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateJobRequest {
    pub org: String,
    pub repo: String,
    pub task_type: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i64,
    pub available_at: Option<DateTime<Utc>>,
    pub budget_usd: Option<f64>,
}

impl CreateJobRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.org.trim().is_empty() {
            return Err("org must not be empty".to_owned());
        }
        if self.repo.trim().is_empty() {
            return Err("repo must not be empty".to_owned());
        }
        if self.task_type.trim().is_empty() {
            return Err("task_type must not be empty".to_owned());
        }
        if self.max_attempts < 1 || self.max_attempts > 100 {
            return Err("max_attempts must be between 1 and 100".to_owned());
        }
        if self.priority < -1000 || self.priority > 1000 {
            return Err("priority must be between -1000 and 1000".to_owned());
        }
        if self.budget_usd.is_some_and(|budget| budget <= 0.0) {
            return Err("budget_usd must be greater than zero".to_owned());
        }
        if self.task_type == crate::slack_run::TASK_TYPE {
            crate::slack_run::SlackAgentRunPayload::from_value(&self.payload)?;
        }
        Ok(())
    }

    pub fn validate_idempotency_key(&self, idempotency_key: Option<&str>) -> Result<(), String> {
        if self.task_type != crate::slack_run::TASK_TYPE {
            return Ok(());
        }

        let payload = crate::slack_run::SlackAgentRunPayload::from_value(&self.payload)?;
        let supplied =
            idempotency_key.ok_or_else(|| "slack_agent_run requires Idempotency-Key".to_owned())?;
        if supplied != payload.run_id.as_str() {
            return Err("slack_agent_run Idempotency-Key must equal payload.run_id".to_owned());
        }
        Ok(())
    }
}

fn default_max_attempts() -> i64 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaimJobRequest {
    pub worker_id: String,
    #[serde(default)]
    pub orgs: Vec<String>,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default = "default_lease_seconds")]
    pub lease_seconds: i64,
}

impl ClaimJobRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.worker_id.trim().is_empty() {
            return Err("worker_id must not be empty".to_owned());
        }
        if !(15..=3600).contains(&self.lease_seconds) {
            return Err("lease_seconds must be between 15 and 3600".to_owned());
        }
        if self.task_types.iter().any(|task| task.trim().is_empty()) {
            return Err("task_types must not contain empty values".to_owned());
        }
        Ok(())
    }

    pub fn accepts(&self, job: &Job) -> bool {
        let org_allowed = self.orgs.is_empty() || self.orgs.iter().any(|org| org == &job.org);
        let full_name = format!("{}/{}", job.org, job.repo);
        let repo_allowed = self.repositories.is_empty()
            || self
                .repositories
                .iter()
                .any(|repo| repo == &job.repo || repo == &full_name);
        let task_allowed = self.task_types.is_empty()
            || self
                .task_types
                .iter()
                .any(|task_type| task_type == &job.task_type);
        org_allowed && repo_allowed && task_allowed
    }
}

fn default_lease_seconds() -> i64 {
    120
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatJobRequest {
    pub worker_id: String,
    /// Monotonic claim generation returned as `job.attempts` by the claim response.
    /// Protected workers must echo it so a stale process cannot renew a later lease.
    #[serde(default)]
    pub lease_attempt: Option<i64>,
    #[serde(default = "default_lease_seconds")]
    pub lease_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompleteJobRequest {
    pub worker_id: String,
    /// Monotonic claim generation returned as `job.attempts` by the claim response.
    /// Protected workers must echo it so a stale process cannot complete a later lease.
    #[serde(default)]
    pub lease_attempt: Option<i64>,
    pub outcome: CompletionOutcome,
    pub result: Option<Value>,
    pub error: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default = "default_retry_delay_seconds")]
    pub retry_delay_seconds: i64,
}

fn default_retry_delay_seconds() -> i64 {
    30
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Succeeded,
    Failed,
}

#[cfg(test)]
mod idempotency_key_tests {
    use serde_json::json;

    use super::*;

    const RUN_ID: &str = "ores-00112233445566778899aabb";

    fn slack_job() -> CreateJobRequest {
        CreateJobRequest {
            org: "ORESoftware".to_owned(),
            repo: "ai-agent-coordinator.rs".to_owned(),
            task_type: crate::slack_run::TASK_TYPE.to_owned(),
            payload: json!({
                "schema_version": 1,
                "run_id": RUN_ID,
                "bridge_workflow_id": "workflow-123",
                "provider": "chatgpt",
                "action": "implement",
                "prompt": "Implement DEN-1231 with tests.",
                "origin": {
                    "workspace_id": "T012345",
                    "channel_id": "C012345",
                    "requester_user_id": "U012345"
                },
                "context": {
                    "trust": "untrusted_channel_context",
                    "selection": "latest_non_bot_channel_messages",
                    "messages": []
                },
                "routing": {
                    "repository": "ORESoftware/ai-agent-coordinator.rs",
                    "linear_team_id": "team-uuid",
                    "linear_project_id": "project-uuid",
                    "linear_run_project_id": "run-project-uuid",
                    "linear_issue": "DEN-1231",
                    "write_policy": "draft_pull_request"
                },
                "broadcast_targets": [
                    "slack_run_thread",
                    "ai_agent_coordinator_job",
                    "ai_agent_bridge_workflow",
                    "linear_run_queue",
                    "github_branch_pr_checks"
                ]
            }),
            priority: 25,
            max_attempts: 3,
            available_at: None,
            budget_usd: Some(2.0),
        }
    }

    #[test]
    fn slack_run_requires_the_exact_run_id_header() {
        let request = slack_job();
        request.validate().unwrap();
        request.validate_idempotency_key(Some(RUN_ID)).unwrap();

        assert_eq!(
            request.validate_idempotency_key(None).unwrap_err(),
            "slack_agent_run requires Idempotency-Key"
        );
        assert_eq!(
            request
                .validate_idempotency_key(Some("ores-aaaaaaaaaaaaaaaaaaaaaaaa"))
                .unwrap_err(),
            "slack_agent_run Idempotency-Key must equal payload.run_id"
        );
        assert!(request
            .validate_idempotency_key(Some(" ores-00112233445566778899aabb"))
            .is_err());
    }

    #[test]
    fn non_slack_jobs_keep_optional_idempotency_keys() {
        let request = CreateJobRequest {
            org: "ORESoftware".to_owned(),
            repo: "ai-agent-coordinator.rs".to_owned(),
            task_type: "code_change".to_owned(),
            payload: json!({"goal": "test"}),
            priority: 0,
            max_attempts: 3,
            available_at: None,
            budget_usd: None,
        };

        request.validate().unwrap();
        request.validate_idempotency_key(None).unwrap();
        request
            .validate_idempotency_key(Some("linear:DEN-1231"))
            .unwrap();
    }
}
