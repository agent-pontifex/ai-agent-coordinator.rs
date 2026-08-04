//! Contract checks for the community coordinator's Agent Pontifex job wire shape.
//!
//! Discovery and version negotiation live in `agent_pontifex_discovery`; this
//! module pins the canonical public protocol source and fails closed if the real
//! coordinator `Job` serialization drifts.

use crate::jobs::Job;
use serde_json::Value;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

pub const PROTOCOL_SOURCE_REPOSITORY: &str = "agent-pontifex/agent-sdk.rs";
pub const PROTOCOL_SOURCE_PATH: &str = "agent-pontifex-protocol/src/lib.rs";
pub const PROTOCOL_SOURCE_REVISION: &str = "6a97c7d9e1cc83ca4976bcf45e63deac6bd32a61";

const JOB_KEYS: &[&str] = &[
    "attempts",
    "available_at",
    "budget_usd",
    "claimed_by",
    "created_at",
    "id",
    "last_error",
    "lease_expires_at",
    "max_attempts",
    "org",
    "payload",
    "priority",
    "repo",
    "result",
    "status",
    "task_type",
    "updated_at",
];

pub fn validate_protocol_source_pin() -> Result<(), ContractError> {
    if PROTOCOL_SOURCE_REPOSITORY != "agent-pontifex/agent-sdk.rs"
        || PROTOCOL_SOURCE_PATH != "agent-pontifex-protocol/src/lib.rs"
        || PROTOCOL_SOURCE_REVISION.len() != 40
        || !PROTOCOL_SOURCE_REVISION
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ContractError::new("public protocol source pin drifted"));
    }
    Ok(())
}

pub fn job_to_protocol_value(job: &Job) -> Result<Value, ContractError> {
    validate_protocol_source_pin()?;
    let value = serde_json::to_value(job)
        .map_err(|error| ContractError::new(format!("unable to serialize job: {error}")))?;
    validate_job_value(&value)?;
    Ok(value)
}

pub fn validate_job_value(value: &Value) -> Result<(), ContractError> {
    let object = value
        .as_object()
        .ok_or_else(|| ContractError::new("job envelope must be an object"))?;
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = JOB_KEYS.iter().copied().collect();
    if actual != expected {
        return Err(ContractError::new("job envelope keys drifted"));
    }

    for key in [
        "id",
        "org",
        "repo",
        "task_type",
        "created_at",
        "updated_at",
        "available_at",
    ] {
        let text = object
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ContractError::new(format!("job {key} must be a string")))?;
        if text.trim().is_empty() {
            return Err(ContractError::new(format!("job {key} must not be empty")));
        }
    }

    for key in ["priority", "attempts", "max_attempts"] {
        if object.get(key).and_then(Value::as_i64).is_none() {
            return Err(ContractError::new(format!("job {key} must be an integer")));
        }
    }

    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| ContractError::new("job status must be a string"))?;
    if !matches!(
        status,
        "queued" | "running" | "succeeded" | "failed" | "cancelled"
    ) {
        return Err(ContractError::new("job status is outside the public enum"));
    }

    for key in ["claimed_by", "lease_expires_at", "last_error"] {
        let candidate = object
            .get(key)
            .ok_or_else(|| ContractError::new(format!("job {key} is missing")))?;
        if !candidate.is_null() && !candidate.is_string() {
            return Err(ContractError::new(format!(
                "job {key} must be a string or null"
            )));
        }
    }

    let budget = object
        .get("budget_usd")
        .ok_or_else(|| ContractError::new("job budget_usd is missing"))?;
    if !budget.is_null() && budget.as_f64().is_none() {
        return Err(ContractError::new("job budget_usd must be numeric or null"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractError {
    message: String,
}

impl ContractError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ContractError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Job, JobStatus};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn sample_job() -> Job {
        let timestamp = Utc.with_ymd_and_hms(2026, 8, 4, 18, 0, 0).single().unwrap();
        Job {
            id: "job-1".to_string(),
            org: "agent-pontifex".to_string(),
            repo: "agent-coordinator.rs".to_string(),
            task_type: "code_change".to_string(),
            payload: json!({"goal": "add protocol negotiation"}),
            priority: 25,
            status: JobStatus::Queued,
            created_at: timestamp,
            updated_at: timestamp,
            available_at: timestamp,
            claimed_by: None,
            lease_expires_at: None,
            attempts: 0,
            max_attempts: 3,
            result: None,
            last_error: None,
            budget_usd: Some(1.5),
        }
    }

    #[test]
    fn real_job_serialization_matches_the_public_envelope() {
        let value = job_to_protocol_value(&sample_job()).unwrap();
        assert_eq!(value["status"], "queued");
        assert_eq!(value["org"], "agent-pontifex");

        let mut drifted = value;
        drifted.as_object_mut().unwrap().remove("lease_expires_at");
        assert!(validate_job_value(&drifted).is_err());
    }
}
