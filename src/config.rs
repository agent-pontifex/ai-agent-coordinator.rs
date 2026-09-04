use std::{collections::HashMap, env, fs, path::Path};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::worker_authority::ProtectedWorkerAuthorityConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub workers: WorkerConfig,
    #[serde(default)]
    pub worker_authority: ProtectedWorkerAuthorityConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub budgets: BudgetConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read configuration from {}", path.display()))?;
        let config: Self = serde_yaml::from_str(&text)
            .with_context(|| format!("failed to parse configuration from {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.models.is_empty() {
            bail!("configuration must define at least one model");
        }
        if self.routing.default_order.is_empty() {
            bail!("routing.default_order must contain at least one model");
        }
        if self.server.max_concurrent_model_requests == 0 {
            bail!("server.max_concurrent_model_requests must be greater than zero");
        }
        if self.workers.default_org_concurrency == 0 || self.workers.default_repo_concurrency == 0 {
            bail!("default worker concurrency limits must be greater than zero");
        }
        if self.budgets.default_org_daily_usd <= 0.0 || self.budgets.default_repo_daily_usd <= 0.0 {
            bail!("default daily budgets must be greater than zero");
        }

        for (model_id, model) in &self.models {
            if !self.providers.contains_key(&model.provider) {
                bail!(
                    "model {model_id:?} refers to unknown provider {:?}",
                    model.provider
                );
            }
            if model.input_cost_per_million_usd < 0.0 || model.output_cost_per_million_usd < 0.0 {
                bail!("model {model_id:?} has a negative price");
            }
        }

        for model_id in &self.routing.default_order {
            if !self.models.contains_key(model_id) {
                bail!("routing.default_order refers to unknown model {model_id:?}");
            }
        }

        for (task, model_ids) in &self.routing.task_orders {
            for model_id in model_ids {
                if !self.models.contains_key(model_id) {
                    bail!("routing.task_orders[{task:?}] refers to unknown model {model_id:?}");
                }
            }
        }

        for (model_id, fallbacks) in &self.routing.fallbacks {
            if !self.models.contains_key(model_id) {
                bail!("routing.fallbacks contains unknown source model {model_id:?}");
            }
            for fallback in fallbacks {
                if !self.models.contains_key(fallback) {
                    bail!("routing.fallbacks[{model_id:?}] refers to unknown model {fallback:?}");
                }
            }
        }

        if self.auth.required && env::var(&self.auth.token_env).is_err() {
            bail!(
                "auth is required but environment variable {} is not set",
                self.auth.token_env
            );
        }

        Ok(())
    }

    pub fn api_token(&self) -> Option<String> {
        env::var(&self.auth.token_env)
            .ok()
            .filter(|v| !v.is_empty())
    }

    pub fn github_webhook_secret(&self) -> Option<String> {
        env::var(&self.github.webhook_secret_env)
            .ok()
            .filter(|v| !v.is_empty())
    }

    pub fn model_candidates(&self, requested: &str, task_type: &str) -> Vec<String> {
        let mut candidates = Vec::new();

        match requested {
            "" | "auto" => {
                if let Some(order) = self.routing.task_orders.get(task_type) {
                    candidates.extend(order.iter().cloned());
                } else {
                    candidates.extend(self.routing.default_order.iter().cloned());
                }
            }
            "local" | "cheap" | "balanced" | "frontier" => {
                if let Some(target_tier) = ModelTier::from_alias(requested) {
                    for rank in (0..=target_tier.rank()).rev() {
                        candidates.extend(
                            self.routing
                                .default_order
                                .iter()
                                .filter(|id| {
                                    self.models
                                        .get(*id)
                                        .map(|model| model.tier.rank() == rank)
                                        .unwrap_or(false)
                                })
                                .cloned(),
                        );
                    }
                }
            }
            concrete => {
                if self.models.contains_key(concrete) {
                    candidates.push(concrete.to_owned());
                    if let Some(fallbacks) = self.routing.fallbacks.get(concrete) {
                        candidates.extend(fallbacks.iter().cloned());
                    }
                }
            }
        }

        if candidates.is_empty() {
            candidates.extend(self.routing.default_order.iter().cloned());
        }

        let mut deduped = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if !deduped.contains(&candidate) {
                deduped.push(candidate);
            }
        }
        deduped
    }

    pub fn org_daily_limit(&self, org: &str) -> f64 {
        self.budgets
            .org_daily_usd
            .get(org)
            .copied()
            .unwrap_or(self.budgets.default_org_daily_usd)
    }

    pub fn repo_daily_limit(&self, org: &str, repo: &str) -> f64 {
        let full_name = format!("{org}/{repo}");
        self.budgets
            .repo_daily_usd
            .get(&full_name)
            .copied()
            .unwrap_or(self.budgets.default_repo_daily_usd)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_model_concurrency")]
    pub max_concurrent_model_requests: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            max_concurrent_model_requests: default_model_concurrency(),
        }
    }
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_owned()
}

fn default_model_concurrency() -> usize {
    16
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_database_url_env")]
    pub url_env: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: None,
            url_env: default_database_url_env(),
        }
    }
}

impl DatabaseConfig {
    pub fn database_url(&self) -> Result<String> {
        self.url
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                env::var(&self.url_env)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| {
                env::var("DATABASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "database URL is required; set database.url, {}, or DATABASE_URL",
                    self.url_env
                )
            })
    }
}

fn default_database_url_env() -> String {
    "AI_AGENT_COORDINATOR_DATABASE_URL".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default = "default_api_token_env")]
    pub token_env: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            required: true,
            token_env: default_api_token_env(),
        }
    }
}

fn default_api_token_env() -> String {
    "COORDINATOR_API_TOKEN".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubConfig {
    #[serde(default = "default_webhook_secret_env")]
    pub webhook_secret_env: String,
    #[serde(default = "default_issue_trigger_labels")]
    pub issue_trigger_labels: Vec<String>,
    #[serde(default = "default_review_trigger_labels")]
    pub review_trigger_labels: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_enqueue_failed_workflows: bool,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            webhook_secret_env: default_webhook_secret_env(),
            issue_trigger_labels: default_issue_trigger_labels(),
            review_trigger_labels: default_review_trigger_labels(),
            auto_enqueue_failed_workflows: true,
        }
    }
}

fn default_webhook_secret_env() -> String {
    "GITHUB_WEBHOOK_SECRET".to_owned()
}

fn default_issue_trigger_labels() -> Vec<String> {
    vec!["agent:run".to_owned()]
}

fn default_review_trigger_labels() -> Vec<String> {
    vec!["agent:review".to_owned()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    #[serde(default = "default_org_worker_concurrency")]
    pub default_org_concurrency: usize,
    #[serde(default = "default_repo_worker_concurrency")]
    pub default_repo_concurrency: usize,
    #[serde(default)]
    pub org_concurrency: HashMap<String, usize>,
    #[serde(default)]
    pub repo_concurrency: HashMap<String, usize>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            default_org_concurrency: default_org_worker_concurrency(),
            default_repo_concurrency: default_repo_worker_concurrency(),
            org_concurrency: HashMap::new(),
            repo_concurrency: HashMap::new(),
        }
    }
}

impl WorkerConfig {
    pub fn org_limit(&self, org: &str) -> usize {
        self.org_concurrency
            .get(org)
            .copied()
            .unwrap_or(self.default_org_concurrency)
    }

    pub fn repo_limit(&self, org: &str, repo: &str) -> usize {
        self.repo_concurrency
            .get(&format!("{org}/{repo}"))
            .copied()
            .unwrap_or(self.default_repo_concurrency)
    }
}

fn default_org_worker_concurrency() -> usize {
    5
}

fn default_repo_worker_concurrency() -> usize {
    2
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub default_order: Vec<String>,
    #[serde(default)]
    pub task_orders: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub fallbacks: HashMap<String, Vec<String>>,
    #[serde(default = "default_true")]
    pub require_repository_context: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            default_order: Vec::new(),
            task_orders: HashMap::new(),
            fallbacks: HashMap::new(),
            require_repository_context: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BudgetConfig {
    #[serde(default = "default_org_budget")]
    pub default_org_daily_usd: f64,
    #[serde(default = "default_repo_budget")]
    pub default_repo_daily_usd: f64,
    #[serde(default)]
    pub org_daily_usd: HashMap<String, f64>,
    #[serde(default)]
    pub repo_daily_usd: HashMap<String, f64>,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            default_org_daily_usd: default_org_budget(),
            default_repo_daily_usd: default_repo_budget(),
            org_daily_usd: HashMap::new(),
            repo_daily_usd: HashMap::new(),
        }
    }
}

fn default_org_budget() -> f64 {
    50.0
}

fn default_repo_budget() -> f64 {
    5.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_max_request_bytes")]
    pub max_request_bytes: usize,
    #[serde(default = "default_true")]
    pub redact_secrets: bool,
    #[serde(default = "default_true")]
    pub deny_remote_when_secrets_cannot_be_redacted: bool,
    #[serde(default = "default_true")]
    pub restricted_requires_local: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            max_request_bytes: default_max_request_bytes(),
            redact_secrets: true,
            deny_remote_when_secrets_cannot_be_redacted: true,
            restricted_requires_local: true,
        }
    }
}

fn default_max_request_bytes() -> usize {
    2 * 1024 * 1024
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub trust: ProviderTrust,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    #[serde(default = "default_provider_timeout")]
    pub timeout_seconds: u64,
}

fn default_provider_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    OpenaiCompatible,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderTrust {
    Local,
    Enterprise,
    #[default]
    Public,
}

impl ProviderTrust {
    pub fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub upstream_model: String,
    #[serde(default)]
    pub tier: ModelTier,
    #[serde(default)]
    pub input_cost_per_million_usd: f64,
    #[serde(default)]
    pub output_cost_per_million_usd: f64,
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ModelConfig {
    pub fn estimated_cost_usd(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 / 1_000_000.0) * self.input_cost_per_million_usd
            + (output_tokens as f64 / 1_000_000.0) * self.output_cost_per_million_usd
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Local,
    #[default]
    Cheap,
    Balanced,
    Frontier,
}

impl ModelTier {
    pub fn rank(self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Cheap => 1,
            Self::Balanced => 2,
            Self::Frontier => 3,
        }
    }

    pub fn from_alias(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "cheap" => Some(Self::Cheap),
            "balanced" => Some(Self::Balanced),
            "frontier" => Some(Self::Frontier),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn explicit_tier_starts_at_requested_tier_then_downgrades() {
        let config: Config = serde_yaml::from_str(
            r#"
auth:
  required: false
routing:
  default_order: [local, cheap, frontier]
providers:
  p:
    base_url: http://localhost
models:
  local:
    provider: p
    upstream_model: local
    tier: local
  cheap:
    provider: p
    upstream_model: cheap
    tier: cheap
  frontier:
    provider: p
    upstream_model: frontier
    tier: frontier
"#,
        )
        .unwrap();

        assert_eq!(
            config.model_candidates("frontier", "general"),
            vec!["frontier", "cheap", "local"]
        );
    }
}
