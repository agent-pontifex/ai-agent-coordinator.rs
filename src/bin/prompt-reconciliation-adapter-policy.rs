//! Fail-closed policy primitives for authenticated prompt reconciliation adapters.

use std::{collections::BTreeSet, fmt, io::Read};

const MAX_RETRY_AFTER_SECONDS: u64 = 300;
const MAX_IDENTIFIER_BYTES: usize = 128;

#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn from_environment(name: &str) -> Result<Self, PolicyError> {
        let value = std::env::var(name)
            .map_err(|_| PolicyError::new("required adapter credential is unavailable"))?;
        if value.trim().is_empty() || value.len() > 16 * 1024 {
            return Err(PolicyError::new(
                "adapter credential is empty or exceeds the configured bound",
            ));
        }
        Ok(Self(value))
    }

    pub fn expose_for_authorization_header(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyError {
    message: &'static str,
}

impl PolicyError {
    pub const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PolicyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    SafeRead,
    Mutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry,
    DoNotRetry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryAllowlist {
    repositories: BTreeSet<String>,
}

impl RepositoryAllowlist {
    pub fn parse(value: &str) -> Result<Self, PolicyError> {
        let mut repositories = BTreeSet::new();
        for raw in value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let (owner, repository) = raw
                .split_once('/')
                .ok_or_else(|| PolicyError::new("allowlist entries must be owner/repository"))?;
            validate_slug(owner)?;
            validate_slug(repository)?;
            if raw.matches('/').count() != 1 {
                return Err(PolicyError::new(
                    "allowlist entries must contain exactly one slash",
                ));
            }
            repositories.insert(repository_key(owner, repository));
        }
        if repositories.is_empty() {
            return Err(PolicyError::new("repository allowlist must not be empty"));
        }
        Ok(Self { repositories })
    }

    pub fn permits(&self, owner: &str, repository: &str) -> bool {
        validate_slug(owner).is_ok()
            && validate_slug(repository).is_ok()
            && self
                .repositories
                .contains(&repository_key(owner, repository))
    }
}

pub fn validate_endpoint(
    endpoint: &str,
    expected_https_host: &str,
    allow_loopback_http: bool,
) -> Result<(), PolicyError> {
    if endpoint.len() > 2_048 || endpoint.contains(['?', '#', '\r', '\n']) {
        return Err(PolicyError::new(
            "adapter endpoint is malformed or contains forbidden URL components",
        ));
    }
    let (scheme, remainder) = endpoint
        .split_once("://")
        .ok_or_else(|| PolicyError::new("adapter endpoint must include a scheme"))?;
    let authority = remainder.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(PolicyError::new(
            "adapter endpoint authority is empty or contains user information",
        ));
    }
    let (host, port) = split_host_port(authority)?;
    if scheme == "https"
        && host.eq_ignore_ascii_case(expected_https_host)
        && port.is_none_or(|value| value == 443)
    {
        return Ok(());
    }
    if scheme == "http" && allow_loopback_http && is_loopback_host(host) && port.is_some() {
        return Ok(());
    }
    Err(PolicyError::new(
        "adapter endpoint must be the pinned HTTPS host or an explicit loopback test server",
    ))
}

pub fn retry_decision(
    operation: OperationKind,
    status: Option<u16>,
    transport_failed: bool,
    completed_attempts: u8,
    maximum_attempts: u8,
) -> RetryDecision {
    if operation == OperationKind::Mutation
        || maximum_attempts == 0
        || completed_attempts >= maximum_attempts
    {
        return RetryDecision::DoNotRetry;
    }
    if transport_failed {
        return RetryDecision::Retry;
    }
    match status {
        Some(408 | 425 | 429 | 500 | 502 | 503 | 504) => RetryDecision::Retry,
        _ => RetryDecision::DoNotRetry,
    }
}

pub fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= MAX_RETRY_AFTER_SECONDS)
}

pub fn read_bounded(mut reader: impl Read, maximum_bytes: usize) -> Result<Vec<u8>, PolicyError> {
    let probe_limit = maximum_bytes
        .checked_add(1)
        .ok_or_else(|| PolicyError::new("response size bound cannot be represented safely"))?;
    let mut bytes = Vec::with_capacity(maximum_bytes.min(64 * 1024));
    let probe_limit = u64::try_from(probe_limit)
        .map_err(|_| PolicyError::new("response size bound exceeds platform limits"))?;
    reader
        .by_ref()
        .take(probe_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| PolicyError::new("could not read adapter response"))?;
    if bytes.len() > maximum_bytes {
        return Err(PolicyError::new(
            "adapter response exceeds the configured byte bound",
        ));
    }
    Ok(bytes)
}

pub fn operation_marker(plan_digest: &str, operation_id: &str) -> Result<String, PolicyError> {
    if !is_lower_hex_digest(plan_digest) {
        return Err(PolicyError::new(
            "operation marker requires a lowercase SHA-256 plan digest",
        ));
    }
    validate_slug(operation_id)?;
    Ok(format!(
        "prompt-reconciliation:{plan_digest}:{operation_id}"
    ))
}

fn validate_slug(value: &str) -> Result<(), PolicyError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(PolicyError::new(
            "repository and operation identifiers must be bounded safe ASCII",
        ))
    }
}

fn repository_key(owner: &str, repository: &str) -> String {
    format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        repository.to_ascii_lowercase()
    )
}

fn split_host_port(authority: &str) -> Result<(&str, Option<u16>), PolicyError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or_else(|| PolicyError::new("IPv6 endpoint authority is malformed"))?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(parse_port(suffix.strip_prefix(':').ok_or_else(|| {
                PolicyError::new("endpoint port separator is malformed")
            })?)?)
        };
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Ok((host, Some(parse_port(port)?))),
        Some(_) => Err(PolicyError::new(
            "IPv6 endpoint hosts must use bracket notation",
        )),
        None => Ok((authority, None)),
    }
}

fn parse_port(value: &str) -> Result<u16, PolicyError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| PolicyError::new("endpoint port is invalid"))?;
    if port == 0 {
        return Err(PolicyError::new("endpoint port must be nonzero"));
    }
    Ok(port)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn main() {
    eprintln!("prompt-reconciliation-adapter-policy is a policy test target; use prompt-intake for reconciliation workflows");
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn secrets_are_redacted_by_debug() {
        let secret = Secret("sensitive-value".to_owned());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret([REDACTED])");
        assert!(!rendered.contains("sensitive-value"));
    }

    #[test]
    fn allowlist_is_case_insensitive_and_exact() {
        let allowlist =
            RepositoryAllowlist::parse("ORESoftware/ai-agent-coordinator.rs, sonus-auris/mobile");
        assert!(allowlist.is_ok());
        let Some(allowlist) = allowlist.ok() else {
            return;
        };
        assert!(allowlist.permits("oresoftware", "AI-AGENT-COORDINATOR.RS"));
        assert!(!allowlist.permits("oresoftware", "other"));
        assert!(!allowlist.permits("evil", "ai-agent-coordinator.rs"));
    }

    #[test]
    fn endpoints_are_pinned_and_redirect_targets_must_be_revalidated() {
        assert!(validate_endpoint("https://api.github.com", "api.github.com", false).is_ok());
        assert!(
            validate_endpoint("https://api.github.com:443/repos", "api.github.com", false).is_ok()
        );
        assert!(
            validate_endpoint("https://api.github.com.evil.test", "api.github.com", false).is_err()
        );
        assert!(
            validate_endpoint("https://token@api.github.com", "api.github.com", false).is_err()
        );
        assert!(validate_endpoint("http://api.github.com", "api.github.com", false).is_err());
        assert!(validate_endpoint("http://127.0.0.1:8080", "api.github.com", true).is_ok());
        assert!(validate_endpoint("http://localhost:8080", "api.github.com", false).is_err());
    }

    #[test]
    fn only_safe_reads_receive_transient_retries() {
        assert_eq!(
            retry_decision(OperationKind::SafeRead, Some(429), false, 1, 3),
            RetryDecision::Retry
        );
        assert_eq!(
            retry_decision(OperationKind::SafeRead, None, true, 1, 3),
            RetryDecision::Retry
        );
        assert_eq!(
            retry_decision(OperationKind::SafeRead, Some(403), false, 1, 3),
            RetryDecision::DoNotRetry
        );
        assert_eq!(
            retry_decision(OperationKind::Mutation, Some(503), false, 1, 3),
            RetryDecision::DoNotRetry
        );
        assert_eq!(
            retry_decision(OperationKind::Mutation, None, true, 1, 3),
            RetryDecision::DoNotRetry
        );
        assert_eq!(
            retry_decision(OperationKind::SafeRead, Some(503), false, 3, 3),
            RetryDecision::DoNotRetry
        );
    }

    #[test]
    fn retry_after_is_numeric_and_bounded() {
        assert_eq!(parse_retry_after_seconds("30"), Some(30));
        assert_eq!(parse_retry_after_seconds("301"), None);
        assert_eq!(
            parse_retry_after_seconds("Wed, 21 Oct 2015 07:28:00 GMT"),
            None
        );
    }

    #[test]
    fn responses_are_bounded_before_parsing() {
        assert_eq!(
            read_bounded(io::Cursor::new(b"hello"), 5).ok(),
            Some(b"hello".to_vec())
        );
        assert!(read_bounded(io::Cursor::new(b"hello!"), 5).is_err());
    }

    #[test]
    fn operation_markers_are_stable_and_nonsecret() {
        let digest = "a".repeat(64);
        assert_eq!(
            operation_marker(&digest, "DEN-1610-op-1").ok(),
            Some(format!("prompt-reconciliation:{digest}:DEN-1610-op-1"))
        );
        assert!(operation_marker("not-a-digest", "DEN-1610-op-1").is_err());
        assert!(operation_marker(&digest, "contains whitespace").is_err());
    }
}
