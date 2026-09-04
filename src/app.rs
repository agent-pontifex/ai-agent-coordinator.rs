use std::{env, sync::Arc};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    config::Config,
    db::Database,
    email_attention::{EmailAttentionAgent, ManualEmailAttentionRunRequest},
    error::AppError,
    gateway::ModelGateway,
    github_admin::{CreateRepositoryRequest, GithubRepositoryAdmin},
    jobs::{
        ClaimJobRequest, CompleteJobRequest, CompletionOutcome, CreateJobRequest,
        HeartbeatJobRequest, Job,
    },
    linear_delivery::{LinearDeliveryRequest, LinearDeliveryWorker},
    providers::ProviderRegistry,
    security::SecretScanner,
    telemetry::{AlertmanagerPayload, TelemetryAutomation, TelemetryError},
    webhooks,
    worker_authority::{
        is_protected_task_type, AuthorizedClaim, AuthorizedWorker, ClaimTaskPolicy,
        WorkerAuthorityError, WorkerAuthorityRegistry,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub database: Database,
    pub gateway: ModelGateway,
    pub github_repository_admin: GithubRepositoryAdmin,
    pub linear_delivery_worker: LinearDeliveryWorker,
    pub telemetry_automation: TelemetryAutomation,
    pub email_attention_agent: EmailAttentionAgent,
    worker_authority: WorkerAuthorityRegistry,
    api_token: Option<String>,
    github_webhook_policy: webhooks::GithubWebhookPolicy,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let config = Arc::new(config);
        let database_url = config.database.database_url()?;
        let database = Database::open(&database_url).await?;
        let providers = ProviderRegistry::from_config(&config)?;
        let scanner = SecretScanner::new()?;
        let gateway = ModelGateway::new(config.clone(), database.clone(), providers, scanner);
        let github_repository_admin = GithubRepositoryAdmin::from_env()?;
        let linear_delivery_worker = LinearDeliveryWorker::from_env(database.clone())?;
        let telemetry_automation = TelemetryAutomation::from_env()?;
        let email_attention_agent = EmailAttentionAgent::from_env(Some(&database_url)).await?;
        let api_token = config.api_token();
        let worker_authority = WorkerAuthorityRegistry::from_config_with_lookup(
            &config.worker_authority,
            api_token.as_deref(),
            |name| env::var(name).ok().filter(|value| !value.is_empty()),
        )
        .map_err(anyhow::Error::new)?;
        let github_webhook_policy =
            webhooks::GithubWebhookPolicy::from_env(config.github_webhook_secret())?;
        Ok(Self {
            config,
            database,
            gateway,
            github_repository_admin,
            linear_delivery_worker,
            telemetry_automation,
            email_attention_agent,
            worker_authority,
            api_token,
            github_webhook_policy,
        })
    }

    fn bearer<'a>(&self, headers: &'a HeaderMap) -> Result<Option<&'a str>, AppError> {
        let Some(value) = headers.get("authorization") else {
            return if self.config.auth.required {
                Err(AppError::Unauthorized)
            } else {
                Ok(None)
            };
        };
        let value = value.to_str().map_err(|_| AppError::Unauthorized)?;
        let bearer = value
            .strip_prefix("Bearer ")
            .filter(|value| !value.is_empty())
            .ok_or(AppError::Unauthorized)?;
        Ok(Some(bearer))
    }

    fn authorize(&self, headers: &HeaderMap) -> Result<(), AppError> {
        if !self.config.auth.required {
            return Ok(());
        }
        let expected = self.api_token.as_deref().ok_or(AppError::Unauthorized)?;
        let supplied = self.bearer(headers)?.ok_or(AppError::Unauthorized)?;
        if bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }

    fn authorize_worker_claim(
        &self,
        headers: &HeaderMap,
        request: &ClaimJobRequest,
    ) -> Result<AuthorizedClaim, AppError> {
        match self.bearer(headers)? {
            Some(bearer) => self
                .worker_authority
                .authorize_claim(bearer, request)
                .map_err(map_worker_authority_error),
            None => {
                if request
                    .task_types
                    .iter()
                    .any(|task| is_protected_task_type(task))
                {
                    return Err(AppError::Forbidden(
                        "protected tasks require a role-scoped worker credential".to_owned(),
                    ));
                }
                Ok(AuthorizedClaim {
                    request: request.clone(),
                    policy: ClaimTaskPolicy::ExcludeProtected,
                    profile: None,
                })
            }
        }
    }

    fn authenticate_worker_bearer<'a>(
        &self,
        headers: &'a HeaderMap,
    ) -> Result<Option<&'a str>, AppError> {
        let bearer = self.bearer(headers)?;
        if let Some(value) = bearer {
            self.worker_authority
                .authenticate_presented_bearer(value)
                .map_err(map_worker_authority_error)?;
        }
        Ok(bearer)
    }

    fn authorize_worker_mutation(
        &self,
        bearer: Option<&str>,
        job: &Job,
        supplied_worker_id: &str,
        supplied_lease_attempt: Option<i64>,
    ) -> Result<AuthorizedWorker, AppError> {
        match bearer {
            Some(bearer) => self
                .worker_authority
                .authorize_job_mutation(bearer, job, supplied_worker_id, supplied_lease_attempt)
                .map_err(map_worker_authority_error),
            None => {
                if is_protected_task_type(&job.task_type) {
                    return Err(AppError::Forbidden(
                        "protected tasks require a role-scoped worker credential".to_owned(),
                    ));
                }
                if supplied_worker_id.trim().is_empty() {
                    return Err(AppError::BadRequest(
                        "worker_id must not be empty".to_owned(),
                    ));
                }
                if supplied_lease_attempt.is_some_and(|lease_attempt| {
                    lease_attempt <= 0 || lease_attempt != job.attempts
                }) {
                    return Err(AppError::Forbidden(
                        "worker supplied a stale or invalid lease_attempt".to_owned(),
                    ));
                }
                Ok(AuthorizedWorker {
                    worker_id: supplied_worker_id.to_owned(),
                    lease_attempt: supplied_lease_attempt,
                    profile: None,
                })
            }
        }
    }
}

fn map_worker_authority_error(error: WorkerAuthorityError) -> AppError {
    match error {
        WorkerAuthorityError::Unauthorized => AppError::Unauthorized,
        WorkerAuthorityError::Forbidden(message) => AppError::Forbidden(message),
        WorkerAuthorityError::InvalidConfiguration(message) => {
            AppError::Internal(anyhow::Error::msg(message))
        }
    }
}

pub fn router(state: AppState) -> Router {
    let request_id_header = HeaderName::from_static("x-request-id");
    let max_request_bytes = state.config.security.max_request_bytes;

    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route(
            crate::agent_pontifex_discovery::DISCOVERY_PATH,
            get(agent_pontifex_descriptor),
        )
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/jobs", post(create_job))
        .route("/v1/jobs/claim", post(claim_job))
        .route("/v1/jobs/:id", get(get_job))
        .route("/v1/jobs/:id/heartbeat", post(heartbeat_job))
        .route("/v1/jobs/:id/complete", post(complete_job))
        .route("/v1/jobs/:id/cancel", post(cancel_job))
        .route("/v1/linear/plan/:id", post(plan_linear_delivery))
        .route("/v1/linear/deliver-next", post(deliver_next_linear_job))
        .route("/v1/email-attention/status", get(email_attention_status))
        .route(
            "/v1/email-attention/run-test",
            post(run_email_attention_test),
        )
        .route(
            "/v1/telemetry/process-next",
            post(process_next_telemetry_incident),
        )
        .route(
            "/v1/telemetry/dispatch-remediation",
            post(dispatch_telemetry_remediation),
        )
        .route("/v1/github/repositories", post(create_github_repository))
        .route("/webhooks/github", post(github_webhook))
        .route("/webhooks/alertmanager", post(alertmanager_webhook))
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(add_no_store))
        .with_state(state)
}

async fn add_no_store(
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

async fn agent_pontifex_descriptor() -> Json<crate::agent_pontifex_discovery::ServiceDescriptor> {
    Json(crate::agent_pontifex_discovery::coordinator_descriptor())
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn ready(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    state.database.ready().await.map_err(AppError::Internal)?;
    Ok(Json(json!({"status": "ready"})))
}

async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    Ok(Json(state.gateway.models_response()))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    let context = state.gateway.context_from_headers(&headers)?;
    let response = state.gateway.chat_completions(context, body).await?;
    Ok(Json(response))
}

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    state.authorize(&headers)?;
    request.validate().map_err(AppError::BadRequest)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    request
        .validate_idempotency_key(idempotency_key)
        .map_err(AppError::BadRequest)?;
    let job = state
        .database
        .create_job(&request, idempotency_key)
        .await
        .map_err(AppError::Internal)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"job": job}))))
}

async fn claim_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClaimJobRequest>,
) -> Result<impl IntoResponse, AppError> {
    let authorized = state.authorize_worker_claim(&headers, &request)?;
    authorized
        .request
        .validate()
        .map_err(AppError::BadRequest)?;
    match state
        .database
        .claim_job_authorized(
            &authorized.request,
            &state.config.workers,
            &authorized.policy,
        )
        .await
        .map_err(AppError::Internal)?
    {
        Some(job) => Ok((StatusCode::OK, Json(json!({"job": job})))),
        None => Ok((StatusCode::NO_CONTENT, Json(Value::Null))),
    }
}

async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    let job = state
        .database
        .get_job(&id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound(format!("job {id}")))?;
    Ok(Json(json!({"job": job})))
}

async fn heartbeat_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<HeartbeatJobRequest>,
) -> Result<Json<Value>, AppError> {
    let bearer = state.authenticate_worker_bearer(&headers)?;
    let current = state
        .database
        .get_job(&id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound(format!("job {id}")))?;
    let authorized = state.authorize_worker_mutation(
        bearer,
        &current,
        &request.worker_id,
        request.lease_attempt,
    )?;
    let job = state
        .database
        .heartbeat_job(
            &id,
            &authorized.worker_id,
            authorized.lease_attempt,
            request.lease_seconds,
        )
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(json!({"job": job})))
}

async fn complete_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut request): Json<CompleteJobRequest>,
) -> Result<Json<Value>, AppError> {
    let bearer = state.authenticate_worker_bearer(&headers)?;
    let current = state
        .database
        .get_job(&id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound(format!("job {id}")))?;
    let authorized = state.authorize_worker_mutation(
        bearer,
        &current,
        &request.worker_id,
        request.lease_attempt,
    )?;
    request.worker_id = authorized.worker_id;
    request.lease_attempt = authorized.lease_attempt;
    let job = state
        .database
        .complete_job(&id, &request)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(json!({"job": job})))
}

async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    let job = state
        .database
        .cancel_job(&id)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(json!({"job": job})))
}

async fn plan_linear_delivery(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    if !state.linear_delivery_worker.enabled() {
        return Err(AppError::BadRequest(
            "Linear delivery is disabled".to_owned(),
        ));
    }
    let job = state
        .database
        .get_job(&id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound(format!("job {id}")))?;
    let report = state
        .linear_delivery_worker
        .plan_job(&job)
        .map_err(|error| AppError::BadRequest(error.public_message))?;
    Ok(Json(json!({"delivery": report, "job": job})))
}

async fn deliver_next_linear_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LinearDeliveryRequest>,
) -> Result<impl IntoResponse, AppError> {
    state.authorize(&headers)?;
    if !state.linear_delivery_worker.enabled() {
        return Err(AppError::BadRequest(
            "Linear delivery is disabled".to_owned(),
        ));
    }
    if state.linear_delivery_worker.dry_run() {
        return Err(AppError::BadRequest(
            "live Linear delivery is blocked while dry-run is enabled; use /v1/linear/plan/:id"
                .to_owned(),
        ));
    }

    let worker_id = request.worker_id.clone();
    let claim = request.into_claim();
    claim.validate().map_err(AppError::BadRequest)?;
    let Some(job) = state
        .database
        .claim_job(&claim, &state.config.workers)
        .await
        .map_err(AppError::Internal)?
    else {
        return Ok((StatusCode::NO_CONTENT, Json(Value::Null)));
    };

    match state.linear_delivery_worker.deliver_job(&job).await {
        Ok(report) => {
            let result =
                serde_json::to_value(&report).map_err(|error| AppError::Internal(error.into()))?;
            let completed = state
                .database
                .complete_job(
                    &job.id,
                    &CompleteJobRequest {
                        worker_id,
                        lease_attempt: Some(job.attempts),
                        outcome: CompletionOutcome::Succeeded,
                        result: Some(result),
                        error: None,
                        retryable: false,
                        retry_delay_seconds: 0,
                    },
                )
                .await
                .map_err(AppError::Internal)?;
            Ok((
                StatusCode::OK,
                Json(json!({"delivery": report, "job": completed})),
            ))
        }
        Err(error) => {
            let retry_delay_seconds = error.retry_after.as_secs().clamp(1, 86_400) as i64;
            let completed = state
                .database
                .complete_job(
                    &job.id,
                    &CompleteJobRequest {
                        worker_id,
                        lease_attempt: Some(job.attempts),
                        outcome: CompletionOutcome::Failed,
                        result: None,
                        error: Some(error.public_message.clone()),
                        retryable: error.retryable,
                        retry_delay_seconds,
                    },
                )
                .await
                .map_err(AppError::Internal)?;
            let status = if error.retryable {
                StatusCode::ACCEPTED
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            Ok((
                status,
                Json(json!({
                    "delivery_error": {
                        "message": error.public_message,
                        "retryable": error.retryable,
                        "retry_after_seconds": retry_delay_seconds,
                    },
                    "job": completed,
                })),
            ))
        }
    }
}

async fn email_attention_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    let status = state
        .email_attention_agent
        .status()
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({"email_attention": status})))
}

async fn run_email_attention_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ManualEmailAttentionRunRequest>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    if !state.email_attention_agent.enabled() {
        return Err(AppError::BadRequest(
            "email-attention agent is disabled".to_owned(),
        ));
    }
    let report = state
        .email_attention_agent
        .run_manual_test(request)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({"email_attention": report})))
}

async fn create_github_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRepositoryRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    state.authorize(&headers)?;
    let result = state
        .github_repository_admin
        .create_repository(request)
        .await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(json!({"repository": result}))))
}

async fn github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    let response = webhooks::process_github_webhook(
        &state.database,
        &headers,
        body,
        &state.github_webhook_policy,
        &state.config.github.issue_trigger_labels,
        &state.config.github.review_trigger_labels,
        state.config.github.auto_enqueue_failed_workflows,
    )
    .await?;
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
struct TelemetryWorkerRequest {
    #[serde(default = "default_telemetry_worker_id")]
    worker_id: String,
}

fn default_telemetry_worker_id() -> String {
    "telemetry-delivery".to_owned()
}

#[derive(Debug, Deserialize)]
struct TelemetryDispatchRequest {
    #[serde(default = "default_telemetry_dispatcher_id")]
    worker_id: String,
    #[serde(default = "default_telemetry_dispatch_limit")]
    limit: usize,
}

fn default_telemetry_dispatcher_id() -> String {
    "telemetry-nightly".to_owned()
}

fn default_telemetry_dispatch_limit() -> usize {
    10
}

async fn alertmanager_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AlertmanagerPayload>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    if !state.telemetry_automation.enabled() {
        return Err(AppError::BadRequest(
            "telemetry automation is disabled".to_owned(),
        ));
    }
    state
        .telemetry_automation
        .authorize_webhook(&headers)
        .map_err(|_| AppError::Unauthorized)?;
    let report = state
        .telemetry_automation
        .ingest_alertmanager(&state.database, payload)
        .await
        .map_err(telemetry_app_error)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"telemetry": report}))))
}

async fn process_next_telemetry_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TelemetryWorkerRequest>,
) -> Result<impl IntoResponse, AppError> {
    state.authorize(&headers)?;
    if !state.telemetry_automation.enabled() {
        return Err(AppError::BadRequest(
            "telemetry automation is disabled".to_owned(),
        ));
    }
    match state
        .telemetry_automation
        .process_next_incident(&state.database, &state.config.workers, &request.worker_id)
        .await
        .map_err(telemetry_app_error)?
    {
        Some(report) => Ok((StatusCode::OK, Json(json!({"telemetry": report})))),
        None => Ok((StatusCode::NO_CONTENT, Json(Value::Null))),
    }
}

async fn dispatch_telemetry_remediation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TelemetryDispatchRequest>,
) -> Result<Json<Value>, AppError> {
    state.authorize(&headers)?;
    if !state.telemetry_automation.enabled() {
        return Err(AppError::BadRequest(
            "telemetry automation is disabled".to_owned(),
        ));
    }
    let report = state
        .telemetry_automation
        .dispatch_remediation_batch(
            &state.database,
            &state.config.workers,
            &request.worker_id,
            request.limit,
        )
        .await
        .map_err(telemetry_app_error)?;
    Ok(Json(json!({"telemetry": report})))
}

fn telemetry_app_error(error: TelemetryError) -> AppError {
    if error.retryable {
        AppError::Internal(anyhow::anyhow!(error.public_message))
    } else {
        AppError::BadRequest(error.public_message)
    }
}
