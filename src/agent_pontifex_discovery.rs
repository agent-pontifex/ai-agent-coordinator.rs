//! Vendor-neutral discovery metadata for the coordinator's leased-job surface.
//!
//! GitHub administration, Linear delivery, Slack ingestion, provider routing,
//! customer tenancy, and Fiducia coordination internals are intentionally not
//! part of this public compatibility document.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub const DISCOVERY_SCHEMA_VERSION: u16 = 1;
pub const CURRENT_PROTOCOL_MAJOR: u16 = 1;
pub const COORDINATOR_PROTOCOL_ID: &str = "agent-pontifex.coordinator";
pub const COORDINATOR_SERVICE_ID: &str = "coordinator";
pub const DISCOVERY_PATH: &str = "/.well-known/agent-pontifex";

const MAX_CAPABILITIES: usize = 256;
const MAX_EXTENSIONS: usize = 64;
const MAX_EXTENSION_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersionRange {
    pub min_major: u16,
    pub max_major: u16,
}

impl ProtocolVersionRange {
    pub const fn current() -> Self {
        Self {
            min_major: CURRENT_PROTOCOL_MAJOR,
            max_major: CURRENT_PROTOCOL_MAJOR,
        }
    }

    pub fn validate(self) -> Result<(), DiscoveryError> {
        if self.min_major == 0 || self.min_major > self.max_major {
            return Err(DiscoveryError::new("invalid protocol major-version range"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescriptor {
    pub schema_version: u16,
    pub protocol: String,
    pub protocol_versions: ProtocolVersionRange,
    pub service: String,
    pub implementation: String,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

impl ServiceDescriptor {
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.schema_version != DISCOVERY_SCHEMA_VERSION {
            return Err(DiscoveryError::new("unsupported discovery schema version"));
        }
        if self.protocol != COORDINATOR_PROTOCOL_ID || self.service != COORDINATOR_SERVICE_ID {
            return Err(DiscoveryError::new(
                "service and protocol identifiers do not match the coordinator contract",
            ));
        }
        self.protocol_versions.validate()?;
        validate_identifier(&self.implementation, "implementation")?;

        if self.capabilities.len() > MAX_CAPABILITIES {
            return Err(DiscoveryError::new("too many advertised capabilities"));
        }
        let mut seen = BTreeSet::new();
        for capability in &self.capabilities {
            validate_identifier(capability, "capability")?;
            if !capability.contains('.') {
                return Err(DiscoveryError::new(
                    "capability identifiers must use a namespace",
                ));
            }
            if !seen.insert(capability.as_str()) {
                return Err(DiscoveryError::new("duplicate capability"));
            }
        }
        let mut sorted = self.capabilities.clone();
        sorted.sort();
        if sorted != self.capabilities {
            return Err(DiscoveryError::new(
                "capabilities must be sorted for deterministic negotiation",
            ));
        }

        if self.extensions.len() > MAX_EXTENSIONS {
            return Err(DiscoveryError::new("too many advertised extensions"));
        }
        for (extension, value) in &self.extensions {
            validate_identifier(extension, "extension")?;
            if !extension.contains('.') {
                return Err(DiscoveryError::new(
                    "extension keys must use a vendor namespace",
                ));
            }
            if serde_json::to_vec(value)
                .map_err(|_| DiscoveryError::new("extension is not serializable"))?
                .len()
                > MAX_EXTENSION_BYTES
            {
                return Err(DiscoveryError::new("extension value is too large"));
            }
        }
        Ok(())
    }
}

pub fn coordinator_descriptor() -> ServiceDescriptor {
    let mut capabilities = vec![
        "coordinator.jobs.cancel".to_string(),
        "coordinator.jobs.claim".to_string(),
        "coordinator.jobs.complete".to_string(),
        "coordinator.jobs.create".to_string(),
        "coordinator.jobs.heartbeat".to_string(),
        "coordinator.jobs.idempotency".to_string(),
        "coordinator.jobs.leases".to_string(),
        "coordinator.jobs.retry".to_string(),
    ];
    capabilities.sort();

    let descriptor = ServiceDescriptor {
        schema_version: DISCOVERY_SCHEMA_VERSION,
        protocol: COORDINATOR_PROTOCOL_ID.to_string(),
        protocol_versions: ProtocolVersionRange::current(),
        service: COORDINATOR_SERVICE_ID.to_string(),
        implementation: "oresoftware.ai-agent-coordinator".to_string(),
        capabilities,
        extensions: BTreeMap::new(),
    };
    debug_assert!(descriptor.validate().is_ok());
    descriptor
}

fn validate_identifier(value: &str, field: &str) -> Result<(), DiscoveryError> {
    if value.is_empty() || value.len() > 128 {
        return Err(DiscoveryError::new(format!(
            "{field} must contain 1 to 128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte))
    {
        return Err(DiscoveryError::new(format!(
            "{field} must use lowercase ASCII identifier characters"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryError {
    message: String,
}

impl DiscoveryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DiscoveryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_exposes_only_the_generic_leased_job_contract() {
        let descriptor = coordinator_descriptor();
        descriptor.validate().unwrap();
        assert_eq!(descriptor.protocol, COORDINATOR_PROTOCOL_ID);
        assert_eq!(descriptor.service, COORDINATOR_SERVICE_ID);
        assert_eq!(
            descriptor.protocol_versions,
            ProtocolVersionRange::current()
        );
        assert!(descriptor.extensions.is_empty());
        assert!(descriptor
            .capabilities
            .iter()
            .all(|capability| capability.starts_with("coordinator.jobs.")));
    }

    #[test]
    fn descriptor_validation_rejects_protocol_and_order_drift() {
        let descriptor = coordinator_descriptor();

        let mut protocol_drift = descriptor.clone();
        protocol_drift.protocol = "agent-pontifex.bridge".to_string();
        assert!(protocol_drift.validate().is_err());

        let mut unsorted = descriptor;
        unsorted.capabilities.reverse();
        assert!(unsorted.validate().is_err());
    }
}
