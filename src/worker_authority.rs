//! Server-side authority contract for protected Agent Pontifex workers.
//!
//! Raw worker bearers are resolved once, hashed with a domain separator, and
//! dropped. Job payloads and caller-supplied `worker_id` values are never an
//! identity source. HTTP and persistence integration can consume the normalized
//! authorization decisions exposed here without duplicating policy.

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::jobs::{ClaimJobRequest, Job};

pub const LINEAR_OPINION_CHATGPT: &str = "linear_opinion_chatgpt";
pub const LINEAR_OPINION_CLAUDE: &str = "linear_opinion_claude";
pub const PR_READINESS_PRIMARY: &str = "pr_readiness_primary";
pub const PR_READINESS_CRITIC: &str = "pr_readiness_critic";
pub const RECONCILIATION_LINEAR_FINALIZER: &str = "reconciliation_linear_finalizer";
pub const RECONCILIATION_GITHUB_FINALIZER: &str = "reconciliation_github_finalizer";

pub const PROTECTED_TASK_TYPES: [&str; 6] = [
    LINEAR_OPINION_CHATGPT,
    LINEAR_OPINION_CLAUDE,
    PR_READINESS_PRIMARY,
    PR_READINESS_CRITIC,
    RECONCILIATION_LINEAR_FINALIZER,
    RECONCILIATION_GITHUB_FINALIZER,
];

const PROVIDER_CALL: &str = "provider:call";
const ATTESTATION_SIGN: &str = "attestation:sign";
const ATTESTATION_VERIFY: &str = "attestation:verify";
const LINEAR_MUTATE: &str = "linear:mutate";
const GITHUB_MUTATE: &str = "github:mutate";
const TOKEN_DOMAIN: &[u8] = b"agent-pontifex.worker-bearer.v1\0";

#[must_use]
pub fn is_protected_task_type(task_type: &str) -> bool {
    PROTECTED_TASK_TYPES.contains(&task_type)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedWorkerAuthorityConfig {
    #[serde(default)]
    pub profiles: Vec<ProtectedWorkerProfileConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedWorkerProfileConfig {
    pub worker_id: String,
    pub token_env: String,
    pub role: ProtectedWorkerRole,
    pub task_types: Vec<String>,
    pub provider: Option<String>,
    pub trust_domain: String,
    pub signing_key_id: Option<String>,
    pub signing_key_fingerprint: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedWorkerRole {
    LinearOpinionChatgpt,
    LinearOpinionClaude,
    PrReadinessPrimary,
    PrReadinessCritic,
    ReconciliationLinearFinalizer,
    ReconciliationGithubFinalizer,
}

impl ProtectedWorkerRole {
    #[must_use]
    pub const fn task_type(self) -> &'static str {
        match self {
            Self::LinearOpinionChatgpt => LINEAR_OPINION_CHATGPT,
            Self::LinearOpinionClaude => LINEAR_OPINION_CLAUDE,
            Self::PrReadinessPrimary => PR_READINESS_PRIMARY,
            Self::PrReadinessCritic => PR_READINESS_CRITIC,
            Self::ReconciliationLinearFinalizer => RECONCILIATION_LINEAR_FINALIZER,
            Self::ReconciliationGithubFinalizer => RECONCILIATION_GITHUB_FINALIZER,
        }
    }

    const fn is_signer(self) -> bool {
        matches!(
            self,
            Self::LinearOpinionChatgpt
                | Self::LinearOpinionClaude
                | Self::PrReadinessPrimary
                | Self::PrReadinessCritic
        )
    }

    fn required_capabilities(self) -> BTreeSet<String> {
        let values = match self {
            Self::LinearOpinionChatgpt
            | Self::LinearOpinionClaude
            | Self::PrReadinessPrimary
            | Self::PrReadinessCritic => [PROVIDER_CALL, ATTESTATION_SIGN],
            Self::ReconciliationLinearFinalizer => [ATTESTATION_VERIFY, LINEAR_MUTATE],
            Self::ReconciliationGithubFinalizer => [ATTESTATION_VERIFY, GITHUB_MUTATE],
        };
        values.into_iter().map(str::to_owned).collect()
    }
}

#[derive(Clone)]
pub struct ProtectedWorkerProfile {
    worker_id: String,
    role: ProtectedWorkerRole,
    task_types: BTreeSet<String>,
    provider: Option<String>,
    trust_domain: String,
    signing_key_id: Option<String>,
    signing_key_fingerprint: Option<String>,
    capabilities: BTreeSet<String>,
    bearer_digest: [u8; 32],
}

impl fmt::Debug for ProtectedWorkerProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedWorkerProfile")
            .field("worker_id", &self.worker_id)
            .field("role", &self.role)
            .field("task_types", &self.task_types)
            .field("provider", &self.provider)
            .field("trust_domain", &self.trust_domain)
            .field("signing_key_id", &self.signing_key_id)
            .field("signing_key_fingerprint", &self.signing_key_fingerprint)
            .field("capabilities", &self.capabilities)
            .field("bearer_digest", &"[redacted sha256 digest]")
            .finish()
    }
}

impl ProtectedWorkerProfile {
    #[must_use]
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    #[must_use]
    pub const fn role(&self) -> ProtectedWorkerRole {
        self.role
    }

    #[must_use]
    pub fn task_types(&self) -> &BTreeSet<String> {
        &self.task_types
    }

    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    #[must_use]
    pub fn trust_domain(&self) -> &str {
        &self.trust_domain
    }

    #[must_use]
    pub fn signing_key_id(&self) -> Option<&str> {
        self.signing_key_id.as_deref()
    }

    #[must_use]
    pub fn signing_key_fingerprint(&self) -> Option<&str> {
        self.signing_key_fingerprint.as_deref()
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }

    #[must_use]
    pub fn allows_task(&self, task_type: &str) -> bool {
        self.task_types.contains(task_type)
    }
}

#[derive(Clone, Default)]
pub struct WorkerAuthorityRegistry {
    profiles: Vec<ProtectedWorkerProfile>,
    admin_digest: Option<[u8; 32]>,
}

impl fmt::Debug for WorkerAuthorityRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerAuthorityRegistry")
            .field("profiles", &self.profiles)
            .field(
                "admin_digest",
                &self.admin_digest.map(|_| "[redacted sha256 digest]"),
            )
            .finish()
    }
}

impl WorkerAuthorityRegistry {
    /// Build the registry from public metadata and a secret lookup.
    ///
    /// The lookup receives only declared environment-variable names. Bearer
    /// values are validated, hashed, and dropped before this function returns.
    pub fn from_config_with_lookup<F>(
        config: &ProtectedWorkerAuthorityConfig,
        admin_bearer: Option<&str>,
        mut secret_lookup: F,
    ) -> Result<Self, WorkerAuthorityError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let admin_digest = match admin_bearer {
            Some(value) => {
                validate_admin_bearer(value)?;
                Some(bearer_digest(value))
            }
            None => None,
        };

        if config.profiles.is_empty() {
            return Ok(Self {
                profiles: Vec::new(),
                admin_digest,
            });
        }

        validate_complete_role_set(&config.profiles)?;

        let mut worker_ids = HashSet::new();
        let mut token_envs = HashSet::new();
        let mut digests = Vec::<[u8; 32]>::new();
        let mut trust_domains = HashSet::new();
        let mut key_ids = HashSet::new();
        let mut key_fingerprints = HashSet::new();
        let mut profiles = Vec::with_capacity(config.profiles.len());

        for raw in &config.profiles {
            validate_identifier(&raw.worker_id, "worker_id", 3, 128)?;
            validate_env_name(&raw.token_env)?;
            validate_identifier(&raw.trust_domain, "trust_domain", 3, 160)?;

            require_unique(
                &mut worker_ids,
                &raw.worker_id,
                "protected worker IDs must be globally unique",
            )?;
            require_unique(
                &mut token_envs,
                &raw.token_env,
                "protected worker token environment names must be globally unique",
            )?;
            require_unique(
                &mut trust_domains,
                &raw.trust_domain,
                "protected worker trust domains must be globally unique",
            )?;

            if raw.task_types.len() != 1 || raw.task_types[0] != raw.role.task_type() {
                return Err(WorkerAuthorityError::InvalidConfiguration(format!(
                    "role {:?} must allow exactly task type {}",
                    raw.role,
                    raw.role.task_type()
                )));
            }
            let task_types = raw.task_types.iter().cloned().collect::<BTreeSet<_>>();

            let provider = raw
                .provider
                .as_deref()
                .map(|value| {
                    validate_identifier(value, "provider", 2, 64)?;
                    Ok::<String, WorkerAuthorityError>(value.to_owned())
                })
                .transpose()?;
            validate_provider_binding(raw.role, provider.as_deref())?;

            let capabilities = raw.capabilities.iter().cloned().collect::<BTreeSet<_>>();
            if capabilities.len() != raw.capabilities.len()
                || capabilities != raw.role.required_capabilities()
            {
                return Err(WorkerAuthorityError::InvalidConfiguration(format!(
                    "role {:?} has an incompatible capability set",
                    raw.role
                )));
            }

            let (signing_key_id, signing_key_fingerprint) = if raw.role.is_signer() {
                let key_id = raw.signing_key_id.as_deref().ok_or_else(|| {
                    WorkerAuthorityError::InvalidConfiguration(format!(
                        "role {:?} requires a signing key ID",
                        raw.role
                    ))
                })?;
                validate_identifier(key_id, "signing_key_id", 3, 160)?;
                let fingerprint = raw.signing_key_fingerprint.as_deref().ok_or_else(|| {
                    WorkerAuthorityError::InvalidConfiguration(format!(
                        "role {:?} requires a canonical public-key fingerprint",
                        raw.role
                    ))
                })?;
                validate_fingerprint(fingerprint)?;
                require_unique(
                    &mut key_ids,
                    key_id,
                    "protected signing key IDs must be globally unique",
                )?;
                require_unique(
                    &mut key_fingerprints,
                    fingerprint,
                    "protected signing public-key fingerprints must be globally unique",
                )?;
                (Some(key_id.to_owned()), Some(fingerprint.to_owned()))
            } else {
                if raw.signing_key_id.is_some() || raw.signing_key_fingerprint.is_some() {
                    return Err(WorkerAuthorityError::InvalidConfiguration(format!(
                        "finalizer role {:?} cannot hold signing authority",
                        raw.role
                    )));
                }
                (None, None)
            };

            let bearer = secret_lookup(&raw.token_env).ok_or_else(|| {
                WorkerAuthorityError::InvalidConfiguration(format!(
                    "protected worker credential environment {} is not configured",
                    raw.token_env
                ))
            })?;
            validate_bearer(&bearer)?;
            let digest = bearer_digest(&bearer);
            if admin_digest
                .as_ref()
                .is_some_and(|admin| bool::from(admin.ct_eq(&digest)))
            {
                return Err(WorkerAuthorityError::InvalidConfiguration(
                    "the coordinator admin bearer cannot alias a protected worker credential"
                        .to_owned(),
                ));
            }
            if digests
                .iter()
                .any(|existing| bool::from(existing.ct_eq(&digest)))
            {
                return Err(WorkerAuthorityError::InvalidConfiguration(
                    "protected workers must use distinct bearer credentials".to_owned(),
                ));
            }
            digests.push(digest);

            profiles.push(ProtectedWorkerProfile {
                worker_id: raw.worker_id.clone(),
                role: raw.role,
                task_types,
                provider,
                trust_domain: raw.trust_domain.clone(),
                signing_key_id,
                signing_key_fingerprint,
                capabilities,
                bearer_digest: digest,
            });
        }

        Ok(Self {
            profiles,
            admin_digest,
        })
    }

    #[must_use]
    pub fn configured(&self) -> bool {
        !self.profiles.is_empty()
    }

    #[must_use]
    pub fn profiles(&self) -> &[ProtectedWorkerProfile] {
        &self.profiles
    }

    pub fn authorize_claim(
        &self,
        presented_bearer: &str,
        request: &ClaimJobRequest,
    ) -> Result<AuthorizedClaim, WorkerAuthorityError> {
        match self.authenticate(presented_bearer)? {
            Credential::Admin => {
                if request
                    .task_types
                    .iter()
                    .any(|task| is_protected_task_type(task))
                {
                    return Err(WorkerAuthorityError::Forbidden(
                        "the coordinator admin credential cannot claim protected tasks".to_owned(),
                    ));
                }
                Ok(AuthorizedClaim {
                    request: request.clone(),
                    policy: ClaimTaskPolicy::ExcludeProtected,
                    profile: None,
                })
            }
            Credential::Worker(profile) => {
                if request.worker_id != profile.worker_id {
                    return Err(WorkerAuthorityError::Forbidden(
                        "caller worker_id does not match the authenticated worker profile"
                            .to_owned(),
                    ));
                }
                if request.task_types.is_empty() {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected workers require an explicit protected task filter".to_owned(),
                    ));
                }
                if request
                    .task_types
                    .iter()
                    .any(|task| !is_protected_task_type(task))
                {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected workers cannot mix protected and unprotected task filters"
                            .to_owned(),
                    ));
                }
                if request
                    .task_types
                    .iter()
                    .any(|task| !profile.allows_task(task))
                {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected worker requested a task outside its server-side profile"
                            .to_owned(),
                    ));
                }

                let mut normalized = request.clone();
                normalized.worker_id = profile.worker_id.clone();
                Ok(AuthorizedClaim {
                    request: normalized,
                    policy: ClaimTaskPolicy::Only(profile.task_types.clone()),
                    profile: Some(profile.clone()),
                })
            }
        }
    }

    /// Reauthorize heartbeat/completion against the exact leased job.
    pub fn authorize_job_mutation(
        &self,
        presented_bearer: &str,
        job: &Job,
        supplied_worker_id: &str,
    ) -> Result<AuthorizedWorker, WorkerAuthorityError> {
        let protected = is_protected_task_type(&job.task_type);
        match self.authenticate(presented_bearer)? {
            Credential::Admin => {
                if protected {
                    return Err(WorkerAuthorityError::Forbidden(
                        "the coordinator admin credential cannot heartbeat or complete protected tasks"
                            .to_owned(),
                    ));
                }
                if supplied_worker_id.trim().is_empty() {
                    return Err(WorkerAuthorityError::Forbidden(
                        "worker_id must not be empty".to_owned(),
                    ));
                }
                Ok(AuthorizedWorker {
                    worker_id: supplied_worker_id.to_owned(),
                    profile: None,
                })
            }
            Credential::Worker(profile) => {
                if !protected {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected worker credentials cannot mutate unprotected jobs".to_owned(),
                    ));
                }
                if supplied_worker_id != profile.worker_id {
                    return Err(WorkerAuthorityError::Forbidden(
                        "caller worker_id does not match the authenticated worker profile"
                            .to_owned(),
                    ));
                }
                if !profile.allows_task(&job.task_type) {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected worker is not authorized for the leased job task".to_owned(),
                    ));
                }
                if job.claimed_by.as_deref() != Some(profile.worker_id.as_str()) {
                    return Err(WorkerAuthorityError::Forbidden(
                        "protected job is not leased to the authenticated worker".to_owned(),
                    ));
                }
                Ok(AuthorizedWorker {
                    worker_id: profile.worker_id.clone(),
                    profile: Some(profile.clone()),
                })
            }
        }
    }

    fn authenticate(&self, presented_bearer: &str) -> Result<Credential<'_>, WorkerAuthorityError> {
        validate_bearer(presented_bearer).map_err(|_| WorkerAuthorityError::Unauthorized)?;
        let digest = bearer_digest(presented_bearer);
        for profile in &self.profiles {
            if bool::from(profile.bearer_digest.ct_eq(&digest)) {
                return Ok(Credential::Worker(profile));
            }
        }
        if self
            .admin_digest
            .as_ref()
            .is_some_and(|admin| bool::from(admin.ct_eq(&digest)))
        {
            return Ok(Credential::Admin);
        }
        Err(WorkerAuthorityError::Unauthorized)
    }
}

enum Credential<'a> {
    Admin,
    Worker(&'a ProtectedWorkerProfile),
}

#[derive(Debug, Clone)]
pub struct AuthorizedClaim {
    pub request: ClaimJobRequest,
    pub policy: ClaimTaskPolicy,
    pub profile: Option<ProtectedWorkerProfile>,
}

#[derive(Debug, Clone)]
pub struct AuthorizedWorker {
    pub worker_id: String,
    pub profile: Option<ProtectedWorkerProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimTaskPolicy {
    ExcludeProtected,
    Only(BTreeSet<String>),
}

impl ClaimTaskPolicy {
    #[must_use]
    pub fn allows(&self, task_type: &str) -> bool {
        match self {
            Self::ExcludeProtected => !is_protected_task_type(task_type),
            Self::Only(allowed) => allowed.contains(task_type),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkerAuthorityError {
    #[error("invalid worker authority configuration: {0}")]
    InvalidConfiguration(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
}

fn validate_complete_role_set(
    profiles: &[ProtectedWorkerProfileConfig],
) -> Result<(), WorkerAuthorityError> {
    let expected: BTreeSet<_> = [
        ProtectedWorkerRole::LinearOpinionChatgpt,
        ProtectedWorkerRole::LinearOpinionClaude,
        ProtectedWorkerRole::PrReadinessPrimary,
        ProtectedWorkerRole::PrReadinessCritic,
        ProtectedWorkerRole::ReconciliationLinearFinalizer,
        ProtectedWorkerRole::ReconciliationGithubFinalizer,
    ]
    .into_iter()
    .collect();
    let actual: BTreeSet<_> = profiles.iter().map(|profile| profile.role).collect();
    if profiles.len() != expected.len() || actual != expected {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "a non-empty registry must configure every protected role exactly once".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider_binding(
    role: ProtectedWorkerRole,
    provider: Option<&str>,
) -> Result<(), WorkerAuthorityError> {
    let valid = match role {
        ProtectedWorkerRole::LinearOpinionChatgpt => provider == Some("openai"),
        ProtectedWorkerRole::LinearOpinionClaude => provider == Some("anthropic"),
        ProtectedWorkerRole::PrReadinessPrimary | ProtectedWorkerRole::PrReadinessCritic => {
            provider.is_some()
        }
        ProtectedWorkerRole::ReconciliationLinearFinalizer
        | ProtectedWorkerRole::ReconciliationGithubFinalizer => provider.is_none(),
    };
    if !valid {
        return Err(WorkerAuthorityError::InvalidConfiguration(format!(
            "role {role:?} has an invalid provider binding"
        )));
    }
    Ok(())
}

fn require_unique(
    values: &mut HashSet<String>,
    value: &str,
    message: &str,
) -> Result<(), WorkerAuthorityError> {
    if !values.insert(value.to_owned()) {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            message.to_owned(),
        ));
    }
    Ok(())
}

fn validate_admin_bearer(value: &str) -> Result<(), WorkerAuthorityError> {
    if value.is_empty() || value.len() > 512 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "the coordinator admin bearer must contain 1-512 visible ASCII bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_bearer(value: &str) -> Result<(), WorkerAuthorityError> {
    if !(32..=512).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "worker bearer credentials must contain 32-512 visible ASCII bytes".to_owned(),
        ));
    }
    Ok(())
}

fn bearer_digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

fn validate_env_name(value: &str) -> Result<(), WorkerAuthorityError> {
    let mut bytes = value.bytes();
    let first = bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_uppercase());
    let rest = bytes.all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit());
    if !first || !rest {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "worker credential environment names must match [A-Z_][A-Z0-9_]*".to_owned(),
        ));
    }
    Ok(())
}

fn validate_identifier(
    value: &str,
    label: &str,
    min: usize,
    max: usize,
) -> Result<(), WorkerAuthorityError> {
    if value.len() < min
        || value.len() > max
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(WorkerAuthorityError::InvalidConfiguration(format!(
            "{label} contains invalid or out-of-range characters"
        )));
    }
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<(), WorkerAuthorityError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "fingerprints must use canonical sha256:<64 lowercase hex> form".to_owned(),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkerAuthorityError::InvalidConfiguration(
            "fingerprints must use canonical sha256:<64 lowercase hex> form".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::jobs::JobStatus;

    const ADMIN: &str = "admin-bearer-00000000000000000000000000000001";

    fn fingerprint(value: u8) -> String {
        format!("sha256:{value:064x}")
    }

    fn profile(
        role: ProtectedWorkerRole,
        worker_id: &str,
        token_env: &str,
        provider: Option<&str>,
        trust_domain: &str,
        key_number: Option<u8>,
    ) -> ProtectedWorkerProfileConfig {
        let (key_id, key_fingerprint, capabilities) = if role.is_signer() {
            (
                Some(format!("key:{worker_id}")),
                key_number.map(fingerprint),
                vec![PROVIDER_CALL.to_owned(), ATTESTATION_SIGN.to_owned()],
            )
        } else {
            let mutation = if role == ProtectedWorkerRole::ReconciliationLinearFinalizer {
                LINEAR_MUTATE
            } else {
                GITHUB_MUTATE
            };
            (
                None,
                None,
                vec![ATTESTATION_VERIFY.to_owned(), mutation.to_owned()],
            )
        };
        ProtectedWorkerProfileConfig {
            worker_id: worker_id.to_owned(),
            token_env: token_env.to_owned(),
            role,
            task_types: vec![role.task_type().to_owned()],
            provider: provider.map(str::to_owned),
            trust_domain: trust_domain.to_owned(),
            signing_key_id: key_id,
            signing_key_fingerprint: key_fingerprint,
            capabilities,
        }
    }

    fn fixture() -> (ProtectedWorkerAuthorityConfig, HashMap<String, String>) {
        let profiles = vec![
            profile(
                ProtectedWorkerRole::LinearOpinionChatgpt,
                "linear-opinion-openai",
                "WORKER_TOKEN_OPENAI",
                Some("openai"),
                "trust:linear-opinion-openai",
                Some(1),
            ),
            profile(
                ProtectedWorkerRole::LinearOpinionClaude,
                "linear-opinion-anthropic",
                "WORKER_TOKEN_ANTHROPIC",
                Some("anthropic"),
                "trust:linear-opinion-anthropic",
                Some(2),
            ),
            profile(
                ProtectedWorkerRole::PrReadinessPrimary,
                "pr-readiness-primary",
                "WORKER_TOKEN_READINESS_PRIMARY",
                Some("openai"),
                "trust:pr-readiness-primary",
                Some(3),
            ),
            profile(
                ProtectedWorkerRole::PrReadinessCritic,
                "pr-readiness-critic",
                "WORKER_TOKEN_READINESS_CRITIC",
                Some("anthropic"),
                "trust:pr-readiness-critic",
                Some(4),
            ),
            profile(
                ProtectedWorkerRole::ReconciliationLinearFinalizer,
                "linear-finalizer",
                "WORKER_TOKEN_LINEAR_FINALIZER",
                None,
                "trust:linear-finalizer",
                None,
            ),
            profile(
                ProtectedWorkerRole::ReconciliationGithubFinalizer,
                "github-finalizer",
                "WORKER_TOKEN_GITHUB_FINALIZER",
                None,
                "trust:github-finalizer",
                None,
            ),
        ];
        let tokens = profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                (
                    profile.token_env.clone(),
                    format!("worker-bearer-{index:02}-000000000000000000000000000001"),
                )
            })
            .collect();
        (ProtectedWorkerAuthorityConfig { profiles }, tokens)
    }

    fn registry() -> (WorkerAuthorityRegistry, HashMap<String, String>) {
        let (config, tokens) = fixture();
        let registry =
            WorkerAuthorityRegistry::from_config_with_lookup(&config, Some(ADMIN), |name| {
                tokens.get(name).cloned()
            })
            .expect("valid registry");
        (registry, tokens)
    }

    fn claim(worker_id: &str, task_types: &[&str]) -> ClaimJobRequest {
        ClaimJobRequest {
            worker_id: worker_id.to_owned(),
            orgs: vec![],
            repositories: vec![],
            task_types: task_types.iter().map(|value| (*value).to_owned()).collect(),
            lease_seconds: 120,
        }
    }

    fn running_job(task_type: &str, claimed_by: Option<&str>) -> Job {
        Job {
            id: "job-1".to_owned(),
            org: "agent-pontifex".to_owned(),
            repo: "ai-agent-coordinator.rs".to_owned(),
            task_type: task_type.to_owned(),
            payload: json!({}),
            priority: 0,
            status: JobStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            available_at: Utc::now(),
            claimed_by: claimed_by.map(str::to_owned),
            lease_expires_at: None,
            attempts: 1,
            max_attempts: 3,
            result: None,
            last_error: None,
            budget_usd: None,
        }
    }

    #[test]
    fn valid_registry_binds_all_roles_without_exposing_bearers() {
        let (registry, tokens) = registry();
        assert!(registry.configured());
        assert_eq!(registry.profiles().len(), PROTECTED_TASK_TYPES.len());
        let debug = format!("{registry:?}");
        for bearer in tokens.values() {
            assert!(!debug.contains(bearer));
        }
        assert!(!debug.contains(ADMIN));
        assert!(debug.contains("[redacted sha256 digest]"));
    }

    #[test]
    fn duplicate_and_admin_aliased_bearers_fail_startup() {
        let (config, mut tokens) = fixture();
        let duplicate = tokens["WORKER_TOKEN_OPENAI"].clone();
        tokens.insert("WORKER_TOKEN_ANTHROPIC".to_owned(), duplicate);
        assert!(
            WorkerAuthorityRegistry::from_config_with_lookup(&config, Some(ADMIN), |name| tokens
                .get(name)
                .cloned(),)
            .unwrap_err()
            .to_string()
            .contains("distinct bearer")
        );

        let (config, mut tokens) = fixture();
        tokens.insert("WORKER_TOKEN_OPENAI".to_owned(), ADMIN.to_owned());
        assert!(
            WorkerAuthorityRegistry::from_config_with_lookup(&config, Some(ADMIN), |name| tokens
                .get(name)
                .cloned(),)
            .unwrap_err()
            .to_string()
            .contains("admin bearer")
        );
    }

    #[test]
    fn same_authority_aliases_are_rejected() {
        let (mut config, tokens) = fixture();
        config.profiles[1].worker_id = config.profiles[0].worker_id.clone();
        assert!(build(&config, &tokens).is_err());

        let (mut config, tokens) = fixture();
        config.profiles[1].trust_domain = config.profiles[0].trust_domain.clone();
        assert!(build(&config, &tokens).is_err());

        let (mut config, tokens) = fixture();
        config.profiles[1].signing_key_id = config.profiles[0].signing_key_id.clone();
        assert!(build(&config, &tokens).is_err());

        let (mut config, tokens) = fixture();
        config.profiles[1].signing_key_fingerprint =
            config.profiles[0].signing_key_fingerprint.clone();
        assert!(build(&config, &tokens).is_err());
    }

    #[test]
    fn provider_and_finalizer_capabilities_cannot_overlap() {
        let (mut config, tokens) = fixture();
        let finalizer = config
            .profiles
            .iter_mut()
            .find(|profile| profile.role == ProtectedWorkerRole::ReconciliationLinearFinalizer)
            .expect("linear finalizer");
        finalizer.provider = Some("openai".to_owned());
        finalizer.signing_key_id = Some("key:bad".to_owned());
        finalizer.signing_key_fingerprint = Some(fingerprint(9));
        finalizer.capabilities.push(PROVIDER_CALL.to_owned());
        assert!(build(&config, &tokens).is_err());
    }

    #[test]
    fn admin_claim_policy_excludes_protected_tasks() {
        let (registry, _) = registry();
        let authorized = registry
            .authorize_claim(ADMIN, &claim("generic-worker", &[]))
            .expect("unprotected admin claim");
        assert!(authorized.policy.allows("code_change"));
        for task_type in PROTECTED_TASK_TYPES {
            assert!(!authorized.policy.allows(task_type));
        }
        assert!(registry
            .authorize_claim(ADMIN, &claim("generic-worker", &[LINEAR_OPINION_CHATGPT]),)
            .is_err());
    }

    #[test]
    fn protected_claim_rejects_impersonation_empty_mixed_and_foreign_filters() {
        let (registry, tokens) = registry();
        let bearer = &tokens["WORKER_TOKEN_OPENAI"];
        let authorized = registry
            .authorize_claim(
                bearer,
                &claim("linear-opinion-openai", &[LINEAR_OPINION_CHATGPT]),
            )
            .expect("authorized protected claim");
        assert_eq!(authorized.request.worker_id, "linear-opinion-openai");
        assert!(authorized.policy.allows(LINEAR_OPINION_CHATGPT));
        assert!(!authorized.policy.allows(LINEAR_OPINION_CLAUDE));

        assert!(registry
            .authorize_claim(bearer, &claim("spoofed", &[LINEAR_OPINION_CHATGPT]))
            .is_err());
        assert!(registry
            .authorize_claim(bearer, &claim("linear-opinion-openai", &[]))
            .is_err());
        assert!(registry
            .authorize_claim(
                bearer,
                &claim(
                    "linear-opinion-openai",
                    &[LINEAR_OPINION_CHATGPT, "code_change"],
                ),
            )
            .is_err());
        assert!(registry
            .authorize_claim(
                bearer,
                &claim("linear-opinion-openai", &[LINEAR_OPINION_CLAUDE]),
            )
            .is_err());
    }

    #[test]
    fn heartbeat_and_completion_reauthorize_exact_task_and_lease() {
        let (registry, tokens) = registry();
        let bearer = &tokens["WORKER_TOKEN_ANTHROPIC"];
        let job = running_job(LINEAR_OPINION_CLAUDE, Some("linear-opinion-anthropic"));
        let authorized = registry
            .authorize_job_mutation(bearer, &job, "linear-opinion-anthropic")
            .expect("authorized protected mutation");
        assert_eq!(authorized.worker_id, "linear-opinion-anthropic");

        assert!(registry
            .authorize_job_mutation(bearer, &job, "spoofed")
            .is_err());
        assert!(registry
            .authorize_job_mutation(
                bearer,
                &running_job(LINEAR_OPINION_CLAUDE, Some("another-worker")),
                "linear-opinion-anthropic",
            )
            .is_err());
        assert!(registry
            .authorize_job_mutation(
                &tokens["WORKER_TOKEN_OPENAI"],
                &job,
                "linear-opinion-openai",
            )
            .is_err());
        assert!(registry
            .authorize_job_mutation(ADMIN, &job, "linear-opinion-anthropic")
            .is_err());
    }

    #[test]
    fn protected_workers_cannot_mutate_unprotected_jobs() {
        let (registry, tokens) = registry();
        assert!(registry
            .authorize_job_mutation(
                &tokens["WORKER_TOKEN_OPENAI"],
                &running_job("code_change", Some("linear-opinion-openai")),
                "linear-opinion-openai",
            )
            .is_err());
        assert_eq!(
            registry
                .authorize_job_mutation(
                    ADMIN,
                    &running_job("code_change", Some("generic-worker")),
                    "generic-worker",
                )
                .expect("legacy unprotected mutation")
                .worker_id,
            "generic-worker"
        );
    }

    #[test]
    fn missing_registry_fails_closed_for_protected_tasks() {
        let registry = WorkerAuthorityRegistry::from_config_with_lookup(
            &ProtectedWorkerAuthorityConfig::default(),
            Some(ADMIN),
            |_| None,
        )
        .expect("empty registry remains valid for unprotected jobs");
        let authorized = registry
            .authorize_claim(ADMIN, &claim("generic-worker", &[]))
            .expect("unprotected claims remain available");
        assert!(authorized.policy.allows("github_push"));
        assert!(!authorized.policy.allows(LINEAR_OPINION_CHATGPT));
        assert!(registry
            .authorize_job_mutation(
                ADMIN,
                &running_job(LINEAR_OPINION_CHATGPT, Some("generic-worker")),
                "generic-worker",
            )
            .is_err());
    }

    #[test]
    fn incomplete_role_set_and_missing_secret_fail_startup() {
        let (mut config, tokens) = fixture();
        config.profiles.pop();
        assert!(build(&config, &tokens).is_err());

        let (config, tokens) = fixture();
        assert!(
            WorkerAuthorityRegistry::from_config_with_lookup(&config, Some(ADMIN), |name| {
                if name == "WORKER_TOKEN_OPENAI" {
                    None
                } else {
                    tokens.get(name).cloned()
                }
            },)
            .is_err()
        );
    }

    fn build(
        config: &ProtectedWorkerAuthorityConfig,
        tokens: &HashMap<String, String>,
    ) -> Result<WorkerAuthorityRegistry, WorkerAuthorityError> {
        WorkerAuthorityRegistry::from_config_with_lookup(config, Some(ADMIN), |name| {
            tokens.get(name).cloned()
        })
    }
}
