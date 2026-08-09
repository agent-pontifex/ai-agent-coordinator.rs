use std::{env, net::IpAddr, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use reqwest::{
    header::{HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT},
    redirect::Policy,
    Client, Method, Response, StatusCode, Url,
};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

const DEFAULT_API_BASE_URL: &str = "https://api.github.com";
const DEFAULT_API_ALLOWED_HOSTS: &str = "api.github.com";
const DEFAULT_API_VERSION: &str = "2022-11-28";
const DEFAULT_USER_AGENT: &str = "ai-agent-coordinator";
const ADMIN_ENABLED_ENV: &str = "GITHUB_REPOSITORY_ADMIN_ENABLED";
const ADMIN_TOKEN_ENV: &str = "GITHUB_REPOSITORY_ADMIN_TOKEN";
const ADMIN_ALLOWED_ORGS_ENV: &str = "GITHUB_REPOSITORY_ADMIN_ALLOWED_ORGS";
const API_BASE_URL_ENV: &str = "GITHUB_API_BASE_URL";
const API_ALLOWED_HOSTS_ENV: &str = "GITHUB_API_ALLOWED_HOSTS";
const API_VERSION_ENV: &str = "GITHUB_API_VERSION";
const USER_AGENT_ENV: &str = "GITHUB_API_USER_AGENT";
const MAX_GITHUB_SUCCESS_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_GITHUB_ERROR_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct GithubRepositoryAdmin {
    client: Client,
    settings: Arc<Settings>,
    token: Option<String>,
}

#[derive(Debug)]
struct Settings {
    enabled: bool,
    allowed_orgs: Vec<String>,
    api_base_url: String,
    api_version: String,
    user_agent: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryVisibility {
    Private,
    Public,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryInitialization {
    Empty,
    Readme,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRepositoryRequest {
    pub organization: String,
    pub name: String,
    pub visibility: RepositoryVisibility,
    pub initialization: RepositoryInitialization,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    #[serde(default)]
    pub confirm_repository: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryCreationResult {
    pub dry_run: bool,
    pub created: bool,
    pub existing: bool,
    pub full_name: String,
    pub visibility: RepositoryVisibility,
    pub initialization: RepositoryInitialization,
    pub description: Option<String>,
    pub api_url: String,
    pub html_url: Option<String>,
    pub repository_id: Option<u64>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Serialize)]
struct GithubCreateRepositoryBody<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    visibility: RepositoryVisibility,
    auto_init: bool,
    has_issues: bool,
    has_projects: bool,
    has_wiki: bool,
    allow_squash_merge: bool,
    allow_merge_commit: bool,
    allow_rebase_merge: bool,
}

#[derive(Debug, Deserialize)]
struct GithubRepositoryResponse {
    id: u64,
    full_name: String,
    private: bool,
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GithubErrorResponse {
    message: String,
}

#[derive(Debug)]
struct ValidatedRequest {
    organization: String,
    name: String,
    full_name: String,
    visibility: RepositoryVisibility,
    initialization: RepositoryInitialization,
    description: Option<String>,
    dry_run: bool,
}

impl GithubRepositoryAdmin {
    pub fn from_env() -> anyhow::Result<Self> {
        let enabled = parse_bool_env(ADMIN_ENABLED_ENV, false)?;
        let allowed_orgs = env::var(ADMIN_ALLOWED_ORGS_ENV)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if enabled && allowed_orgs.is_empty() {
            bail!(
                "{ADMIN_ENABLED_ENV} is true but {ADMIN_ALLOWED_ORGS_ENV} is empty; live repository creation must be organization-allowlisted"
            );
        }

        let token = env::var(ADMIN_TOKEN_ENV)
            .ok()
            .filter(|value| !value.is_empty());
        if enabled && token.is_none() {
            bail!(
                "{ADMIN_ENABLED_ENV} is true but {ADMIN_TOKEN_ENV} is not set; use a short-lived GitHub App installation token"
            );
        }
        if token
            .as_deref()
            .is_some_and(|value| !is_safe_bearer_token(value))
        {
            bail!("{ADMIN_TOKEN_ENV} must be a bounded, whitespace-free HTTP bearer credential");
        }

        let api_base_url = env::var(API_BASE_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_API_BASE_URL.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let allowed_api_hosts = env::var(API_ALLOWED_HOSTS_ENV)
            .unwrap_or_else(|_| DEFAULT_API_ALLOWED_HOSTS.to_owned());
        let allowed_api_hosts = parse_api_host_allowlist(&allowed_api_hosts)?;
        if !is_safe_api_base_url(&api_base_url, &allowed_api_hosts) {
            bail!(
                "{API_BASE_URL_ENV} must use HTTPS to a host in {API_ALLOWED_HOSTS_ENV}, except exact loopback HTTP is allowed for local tests"
            );
        }

        let api_version = env::var(API_VERSION_ENV)
            .unwrap_or_else(|_| DEFAULT_API_VERSION.to_owned())
            .trim()
            .to_owned();
        if api_version.is_empty()
            || api_version.len() > 32
            || HeaderValue::from_bytes(api_version.as_bytes()).is_err()
        {
            bail!("{API_VERSION_ENV} must be a valid non-empty HTTP header value");
        }

        let user_agent = env::var(USER_AGENT_ENV)
            .unwrap_or_else(|_| DEFAULT_USER_AGENT.to_owned())
            .trim()
            .to_owned();
        if user_agent.is_empty()
            || user_agent.len() > 128
            || HeaderValue::from_bytes(user_agent.as_bytes()).is_err()
        {
            bail!(
                "{USER_AGENT_ENV} must be a valid HTTP header value between 1 and 128 characters"
            );
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none())
            .build()
            .context("failed to construct GitHub repository administration client")?;

        Ok(Self {
            client,
            settings: Arc::new(Settings {
                enabled,
                allowed_orgs,
                api_base_url,
                api_version,
                user_agent,
            }),
            token,
        })
    }

    pub async fn create_repository(
        &self,
        request: CreateRepositoryRequest,
    ) -> Result<RepositoryCreationResult, AppError> {
        let request = self.validate_request(request)?;
        let api_url = format!(
            "{}/repos/{}/{}",
            self.settings.api_base_url, request.organization, request.name
        );

        if request.dry_run {
            return Ok(RepositoryCreationResult {
                dry_run: true,
                created: false,
                existing: false,
                full_name: request.full_name,
                visibility: request.visibility,
                initialization: request.initialization,
                description: request.description,
                api_url,
                html_url: None,
                repository_id: None,
                default_branch: None,
            });
        }

        if let Some(existing) = self.fetch_repository(&api_url).await? {
            validate_repository_response(&request, &existing)?;
            return Ok(result_from_github(
                request,
                api_url,
                &self.settings.api_base_url,
                existing,
                false,
                true,
            ));
        }

        let create_url = format!(
            "{}/orgs/{}/repos",
            self.settings.api_base_url, request.organization
        );
        let body = GithubCreateRepositoryBody {
            name: &request.name,
            description: request.description.as_deref(),
            visibility: request.visibility,
            auto_init: matches!(request.initialization, RepositoryInitialization::Readme),
            has_issues: true,
            has_projects: false,
            has_wiki: false,
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: false,
        };
        let response = self
            .authorized_request(Method::POST, &create_url)?
            .json(&body)
            .send()
            .await
            .map_err(|error| AppError::Upstream(format!("GitHub request failed: {error}")))?;

        let status = response.status();
        if status != StatusCode::CREATED {
            let original_error = map_github_error(response, self.token.as_deref()).await;
            if matches!(
                status,
                StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
            ) {
                if let Some(existing) = self
                    .fetch_repository(&api_url)
                    .await
                    .ok()
                    .flatten()
                    .filter(|existing| validate_repository_response(&request, existing).is_ok())
                {
                    return Ok(result_from_github(
                        request,
                        api_url,
                        &self.settings.api_base_url,
                        existing,
                        false,
                        true,
                    ));
                }
            }
            return Err(original_error);
        }

        let repository = parse_repository_response(response).await?;
        validate_repository_response(&request, &repository)?;

        Ok(result_from_github(
            request,
            api_url,
            &self.settings.api_base_url,
            repository,
            true,
            false,
        ))
    }

    fn validate_request(
        &self,
        request: CreateRepositoryRequest,
    ) -> Result<ValidatedRequest, AppError> {
        validate_organization(&request.organization)?;
        validate_repository_name(&request.name)?;

        if !self
            .settings
            .allowed_orgs
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(request.organization.as_str()))
        {
            return Err(AppError::Forbidden(format!(
                "organization {:?} is not present in {}",
                request.organization, ADMIN_ALLOWED_ORGS_ENV
            )));
        }

        let description = request
            .description
            .map(|description| description.trim().to_owned())
            .filter(|description| !description.is_empty());
        if description
            .as_ref()
            .is_some_and(|description| description.chars().count() > 350)
        {
            return Err(AppError::BadRequest(
                "repository description must not exceed 350 characters".to_owned(),
            ));
        }
        if description
            .as_ref()
            .is_some_and(|description| description.chars().any(char::is_control))
        {
            return Err(AppError::BadRequest(
                "repository description must not contain control characters".to_owned(),
            ));
        }

        let full_name = format!("{}/{}", request.organization, request.name);
        if !request.dry_run {
            if !self.settings.enabled {
                return Err(AppError::Forbidden(format!(
                    "live repository creation is disabled; set {ADMIN_ENABLED_ENV}=true after reviewing a dry run"
                )));
            }
            if self.token.is_none() {
                return Err(AppError::Forbidden(format!(
                    "live repository creation requires {ADMIN_TOKEN_ENV}"
                )));
            }
            if request.confirm_repository.as_deref() != Some(full_name.as_str()) {
                return Err(AppError::BadRequest(format!(
                    "live repository creation requires confirm_repository to equal {full_name:?}"
                )));
            }
        }

        Ok(ValidatedRequest {
            organization: request.organization,
            name: request.name,
            full_name,
            visibility: request.visibility,
            initialization: request.initialization,
            description,
            dry_run: request.dry_run,
        })
    }

    async fn fetch_repository(
        &self,
        api_url: &str,
    ) -> Result<Option<GithubRepositoryResponse>, AppError> {
        let response = self
            .authorized_request(Method::GET, api_url)?
            .send()
            .await
            .map_err(|error| AppError::Upstream(format!("GitHub request failed: {error}")))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(map_github_error(response, self.token.as_deref()).await);
        }

        parse_repository_response(response).await.map(Some)
    }

    fn authorized_request(
        &self,
        method: Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, AppError> {
        let token = self.token.as_deref().ok_or_else(|| {
            AppError::Forbidden(format!(
                "GitHub repository administration token {} is unavailable",
                ADMIN_TOKEN_ENV
            ))
        })?;

        Ok(self
            .client
            .request(method, url)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, self.settings.user_agent.as_str())
            .header("X-GitHub-Api-Version", self.settings.api_version.as_str())
            .header(AUTHORIZATION, format!("Bearer {token}")))
    }
}

fn validate_repository_response(
    request: &ValidatedRequest,
    repository: &GithubRepositoryResponse,
) -> Result<(), AppError> {
    if repository.id == 0 {
        return Err(AppError::Upstream(
            "GitHub returned an invalid zero repository identifier".to_owned(),
        ));
    }
    if !repository
        .full_name
        .eq_ignore_ascii_case(&request.full_name)
    {
        return Err(AppError::Upstream(format!(
            "GitHub returned repository {:?} while {:?} was requested",
            repository.full_name, request.full_name
        )));
    }
    if repository.default_branch.len() > 255
        || repository.default_branch.chars().any(char::is_control)
    {
        return Err(AppError::Upstream(
            "GitHub returned an invalid default branch".to_owned(),
        ));
    }

    let actual_visibility = if repository.private {
        RepositoryVisibility::Private
    } else {
        RepositoryVisibility::Public
    };
    if actual_visibility != request.visibility {
        return Err(AppError::BadRequest(format!(
            "repository {} already exists with {:?} visibility, not {:?}",
            request.full_name, actual_visibility, request.visibility
        )));
    }

    Ok(())
}

fn result_from_github(
    request: ValidatedRequest,
    api_url: String,
    api_base_url: &str,
    repository: GithubRepositoryResponse,
    created: bool,
    existing: bool,
) -> RepositoryCreationResult {
    let html_url = canonical_repository_html_url(api_base_url, &repository.full_name);
    RepositoryCreationResult {
        dry_run: false,
        created,
        existing,
        full_name: repository.full_name,
        visibility: request.visibility,
        initialization: request.initialization,
        description: request.description,
        api_url,
        html_url,
        repository_id: Some(repository.id),
        default_branch: if repository.default_branch.is_empty() {
            None
        } else {
            Some(repository.default_branch)
        },
    }
}

async fn parse_repository_response(
    mut response: Response,
) -> Result<GithubRepositoryResponse, AppError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GITHUB_SUCCESS_RESPONSE_BYTES as u64)
    {
        return Err(AppError::Upstream(format!(
            "GitHub repository response exceeded {MAX_GITHUB_SUCCESS_RESPONSE_BYTES} bytes"
        )));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::Upstream(format!("GitHub response body failed: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_GITHUB_SUCCESS_RESPONSE_BYTES {
            return Err(AppError::Upstream(format!(
                "GitHub repository response exceeded {MAX_GITHUB_SUCCESS_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice::<GithubRepositoryResponse>(&body).map_err(|error| {
        AppError::Upstream(format!(
            "GitHub returned an invalid repository response: {error}"
        ))
    })
}

async fn read_bounded_github_error_body(mut response: Response) -> String {
    let mut body = Vec::new();
    while body.len() < MAX_GITHUB_ERROR_RESPONSE_BYTES {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => break,
        };
        let remaining = MAX_GITHUB_ERROR_RESPONSE_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&body).into_owned()
}

async fn map_github_error(response: Response, token: Option<&str>) -> AppError {
    let status = response.status();
    let body = read_bounded_github_error_body(response).await;
    let message = serde_json::from_str::<GithubErrorResponse>(&body)
        .map(|error| error.message)
        .unwrap_or_else(|_| "GitHub rejected the repository administration request".to_owned());
    let message = sanitize_github_error_message(&message, token);

    match status {
        StatusCode::UNAUTHORIZED => AppError::Upstream(format!(
            "GitHub rejected the repository administration credential: {message}"
        )),
        StatusCode::FORBIDDEN => AppError::Forbidden(format!(
            "GitHub denied repository administration: {message}"
        )),
        StatusCode::UNPROCESSABLE_ENTITY | StatusCode::CONFLICT => {
            AppError::BadRequest(format!("GitHub rejected repository settings: {message}"))
        }
        _ => AppError::Upstream(format!(
            "GitHub repository administration failed with HTTP {status}: {message}"
        )),
    }
}

fn canonical_repository_html_url(api_base_url: &str, full_name: &str) -> Option<String> {
    let mut url = Url::parse(api_base_url).ok()?;
    if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.github.com"))
    {
        url = Url::parse("https://github.com/").ok()?;
    } else {
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
    }
    url.join(full_name)
        .ok()
        .map(|value| value.to_string().trim_end_matches('/').to_owned())
}

fn sanitize_github_error_message(message: &str, token: Option<&str>) -> String {
    let redacted = token.filter(|value| !value.is_empty()).map_or_else(
        || message.to_owned(),
        |value| message.replace(value, "[REDACTED]"),
    );
    let mut sanitized = String::with_capacity(redacted.len().min(500));
    let mut previous_was_space = false;
    for character in redacted.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if character.is_whitespace() {
            if previous_was_space {
                continue;
            }
            previous_was_space = true;
            sanitized.push(' ');
        } else {
            previous_was_space = false;
            sanitized.push(character);
        }
        if sanitized.len() >= 500 {
            break;
        }
    }
    sanitized.trim().to_owned()
}

fn is_safe_bearer_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_whitespace)
        && HeaderValue::from_bytes(format!("Bearer {value}").as_bytes()).is_ok()
}

fn validate_organization(value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > 39 {
        return Err(AppError::BadRequest(
            "organization must be between 1 and 39 ASCII characters".to_owned(),
        ));
    }
    if value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AppError::BadRequest(
            "organization must contain only ASCII letters, numbers, and single interior hyphens"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_repository_name(value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > 100 {
        return Err(AppError::BadRequest(
            "repository name must be between 1 and 100 ASCII characters".to_owned(),
        ));
    }
    if matches!(value, "." | "..")
        || value.ends_with(".git")
        || value.starts_with('.')
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AppError::BadRequest(
            "repository name must contain only ASCII letters, numbers, hyphens, underscores, and interior dots"
                .to_owned(),
        ));
    }
    Ok(())
}

fn parse_bool_env(name: &str, default: bool) -> anyhow::Result<bool> {
    let Ok(value) = env::var(name) else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be one of true/false, 1/0, yes/no, or on/off"),
    }
}

fn parse_api_host_allowlist(value: &str) -> anyhow::Result<Vec<String>> {
    let hosts = value
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| host.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if hosts.is_empty() {
        bail!("{API_ALLOWED_HOSTS_ENV} must contain at least one exact API host");
    }
    if hosts.iter().any(|host| !is_valid_api_host(host)) {
        bail!(
            "{API_ALLOWED_HOSTS_ENV} must contain only exact DNS names or IP literals without ports"
        );
    }
    Ok(hosts)
}

fn is_valid_api_host(value: &str) -> bool {
    if value.parse::<IpAddr>().is_ok() {
        return true;
    }
    if value.is_empty() || value.len() > 253 || value.starts_with('.') || value.ends_with('.') {
        return false;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn normalized_url_host(value: &str) -> &str {
    value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(value)
}

fn is_safe_api_base_url(value: &str, allowed_https_hosts: &[String]) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let authority = value
        .split_once("://")
        .map(|(_, remainder)| remainder.split(['/', '?', '#']).next().unwrap_or(remainder))
        .unwrap_or_default();
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }

    match url.scheme() {
        "https" => url.host_str().is_some_and(|host| {
            let host = normalized_url_host(host);
            allowed_https_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
        }),
        "http" => url.host_str().is_some_and(|host| {
            let host = normalized_url_host(host);
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }),
        _ => false,
    }
}

fn default_dry_run() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin(enabled: bool, token: Option<&str>, allowed_orgs: &[&str]) -> GithubRepositoryAdmin {
        GithubRepositoryAdmin {
            client: Client::new(),
            settings: Arc::new(Settings {
                enabled,
                allowed_orgs: allowed_orgs
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                api_base_url: DEFAULT_API_BASE_URL.to_owned(),
                api_version: DEFAULT_API_VERSION.to_owned(),
                user_agent: DEFAULT_USER_AGENT.to_owned(),
            }),
            token: token.map(str::to_owned),
        }
    }

    fn request() -> CreateRepositoryRequest {
        CreateRepositoryRequest {
            organization: "declarative-migrations".to_owned(),
            name: "declarative-migrations-monorepo".to_owned(),
            visibility: RepositoryVisibility::Private,
            initialization: RepositoryInitialization::Readme,
            description: Some("Organization monorepo".to_owned()),
            dry_run: true,
            confirm_repository: None,
        }
    }

    #[test]
    fn dry_run_is_the_deserialization_default() {
        let request: CreateRepositoryRequest = serde_json::from_value(serde_json::json!({
            "organization": "declarative-migrations",
            "name": "declarative-migrations-monorepo",
            "visibility": "private",
            "initialization": "readme"
        }))
        .unwrap();
        assert!(request.dry_run);
    }

    #[test]
    fn dry_run_does_not_require_live_mode_or_token() {
        let validated = admin(false, None, &["declarative-migrations"])
            .validate_request(request())
            .unwrap();
        assert!(validated.dry_run);
        assert_eq!(
            validated.full_name,
            "declarative-migrations/declarative-migrations-monorepo"
        );
    }

    #[test]
    fn live_mode_requires_exact_repository_confirmation() {
        let mut request = request();
        request.dry_run = false;
        let error = admin(true, Some("ephemeral-token"), &["declarative-migrations"])
            .validate_request(request)
            .unwrap_err();
        assert!(error.to_string().contains("confirm_repository"));
    }

    #[test]
    fn organization_allowlist_is_case_insensitive() {
        let validated = admin(false, None, &["Declarative-Migrations"])
            .validate_request(request())
            .unwrap();
        assert_eq!(validated.organization, "declarative-migrations");
    }

    #[test]
    fn rejects_unlisted_organizations() {
        let error = admin(false, None, &["oresoftware"])
            .validate_request(request())
            .unwrap_err();
        assert!(matches!(error, AppError::Forbidden(_)));
    }

    #[test]
    fn rejects_repository_names_that_can_escape_a_url_path() {
        let mut request = request();
        request.name = "../other".to_owned();
        let error = admin(false, None, &["declarative-migrations"])
            .validate_request(request)
            .unwrap_err();
        assert!(matches!(error, AppError::BadRequest(_)));
    }

    #[test]
    fn configured_header_values_reject_controls() {
        assert!(HeaderValue::from_bytes(b"2022-11-28").is_ok());
        assert!(HeaderValue::from_bytes(b"ai-agent-coordinator-browser-e2e").is_ok());
        assert!(HeaderValue::from_bytes(b"invalid\nvalue").is_err());
    }

    #[test]
    fn api_base_url_allows_allowlisted_https_and_exact_loopback_http() {
        let allowed = vec![
            "api.github.com".to_owned(),
            "github.example.test".to_owned(),
            "::1".to_owned(),
        ];
        for value in [
            "https://api.github.com",
            "https://github.example.test/api/v3",
            "https://[::1]/api/v3",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://127.42.0.1:8080",
            "http://[::1]:8080/api/v3",
        ] {
            assert!(
                is_safe_api_base_url(value, &allowed),
                "expected {value:?} to be safe"
            );
        }
    }

    #[test]
    fn api_base_url_rejects_prefix_bypasses_and_ambiguous_authorities() {
        let allowed = vec!["api.github.com".to_owned()];
        for value in [
            "http://localhost.attacker.example",
            "http://127.0.0.1.attacker.example",
            "http://localhost@attacker.example",
            "http://127.0.0.1@attacker.example",
            "http://192.0.2.10",
            "https://attacker.example",
            "https://api.github.com.attacker.example",
            "https://user:password@api.github.com",
            "https://@api.github.com",
            "https://api.github.com?token=unexpected",
            "https://api.github.com/#fragment",
            "file:///tmp/github-api",
            "not a URL",
        ] {
            assert!(
                !is_safe_api_base_url(value, &allowed),
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn api_host_allowlist_requires_exact_valid_hosts() {
        assert_eq!(
            parse_api_host_allowlist("api.github.com, GitHub.Example.Test").unwrap(),
            vec![
                "api.github.com".to_owned(),
                "github.example.test".to_owned(),
            ]
        );
        for value in [
            "",
            "api.github.com:443",
            "https://api.github.com",
            ".example",
            "example.",
        ] {
            assert!(
                parse_api_host_allowlist(value).is_err(),
                "expected {value:?} to fail"
            );
        }
    }

    #[test]
    fn repository_response_must_match_the_requested_identity() {
        let validated = admin(false, None, &["declarative-migrations"])
            .validate_request(request())
            .unwrap();
        let response = GithubRepositoryResponse {
            id: 1,
            full_name: "other/repository".to_owned(),
            private: true,
            default_branch: "main".to_owned(),
        };
        let error = validate_repository_response(&validated, &response).unwrap_err();
        assert!(matches!(error, AppError::Upstream(_)));
    }

    #[test]
    fn repository_response_must_match_the_requested_visibility() {
        let validated = admin(false, None, &["declarative-migrations"])
            .validate_request(request())
            .unwrap();
        let response = GithubRepositoryResponse {
            id: 1,
            full_name: validated.full_name.clone(),
            private: false,
            default_branch: "main".to_owned(),
        };
        let error = validate_repository_response(&validated, &response).unwrap_err();
        assert!(matches!(error, AppError::BadRequest(_)));
    }

    #[test]
    fn repository_response_rejects_zero_ids_and_controlled_branches() {
        let validated = admin(false, None, &["declarative-migrations"])
            .validate_request(request())
            .unwrap();
        let zero_id = GithubRepositoryResponse {
            id: 0,
            full_name: validated.full_name.clone(),
            private: true,
            default_branch: "main".to_owned(),
        };
        assert!(matches!(
            validate_repository_response(&validated, &zero_id),
            Err(AppError::Upstream(_))
        ));

        let invalid_branch = GithubRepositoryResponse {
            id: 1,
            full_name: validated.full_name.clone(),
            private: true,
            default_branch: "main\nleak".to_owned(),
        };
        assert!(matches!(
            validate_repository_response(&validated, &invalid_branch),
            Err(AppError::Upstream(_))
        ));
    }

    #[test]
    fn bearer_tokens_must_be_bounded_and_header_safe() {
        assert!(is_safe_bearer_token("github-app-installation-token"));
        assert!(!is_safe_bearer_token("token with spaces"));
        assert!(!is_safe_bearer_token("token\nwith-control"));
        assert!(!is_safe_bearer_token(&"x".repeat(4097)));
    }

    #[test]
    fn github_error_messages_redact_the_credential_and_controls() {
        let sanitized = sanitize_github_error_message(
            "credential secret-token\nwas rejected",
            Some("secret-token"),
        );
        assert_eq!(sanitized, "credential [REDACTED] was rejected");
    }

    #[test]
    fn repository_html_urls_are_derived_from_the_trusted_api_base() {
        assert_eq!(
            canonical_repository_html_url("https://api.github.com", "owner/repository"),
            Some("https://github.com/owner/repository".to_owned())
        );
        assert_eq!(
            canonical_repository_html_url(
                "https://github.enterprise.example/api/v3",
                "owner/repository"
            ),
            Some("https://github.enterprise.example/owner/repository".to_owned())
        );
    }

    #[test]
    fn creation_body_disables_rebase_merges() {
        let request = request();
        let body = GithubCreateRepositoryBody {
            name: &request.name,
            description: request.description.as_deref(),
            visibility: request.visibility,
            auto_init: true,
            has_issues: true,
            has_projects: false,
            has_wiki: false,
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: false,
        };
        let value = serde_json::to_value(body).unwrap();
        assert_eq!(value["allow_rebase_merge"], false);
        assert_eq!(value["visibility"], "private");
    }
}
