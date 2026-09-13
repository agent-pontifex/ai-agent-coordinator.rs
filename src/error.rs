use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;
use tracing::error;

use crate::lease::LeaseError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("all model routes failed: {0}")]
    Upstream(String),
    #[error("lease rejected: {0}")]
    Lease(LeaseError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// Maps a lease-bound mutation failure: explicit lease rejections keep
    /// their stable code, validation failures stay `bad_request`.
    pub fn from_lease_mutation(error: anyhow::Error) -> Self {
        match error.downcast_ref::<LeaseError>() {
            Some(lease) => Self::Lease(*lease),
            None => Self::BadRequest(error.to_string()),
        }
    }

    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Lease(LeaseError::NotFound) => {
                (StatusCode::NOT_FOUND, LeaseError::NotFound.code())
            }
            Self::Lease(lease) => (StatusCode::CONFLICT, lease.code()),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::BudgetExceeded(_) => (StatusCode::PAYMENT_REQUIRED, "budget_exceeded"),
            Self::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_failure"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        if matches!(&self, Self::Internal(_)) {
            error!(error = ?self, "request failed");
        }

        let message = match &self {
            Self::Internal(_) => "internal server error".to_owned(),
            _ => self.to_string(),
        };

        (
            status,
            Json(json!({
                "error": {
                    "code": code,
                    "message": message,
                }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobStatus;

    #[test]
    fn lease_rejections_map_to_explicit_statuses() {
        let cases = [
            (LeaseError::Expired, StatusCode::CONFLICT, "lease_expired"),
            (
                LeaseError::HeldByAnotherWorker,
                StatusCode::CONFLICT,
                "lease_not_held",
            ),
            (
                LeaseError::NotRunning(JobStatus::Cancelled),
                StatusCode::CONFLICT,
                "job_not_running",
            ),
            (LeaseError::NotFound, StatusCode::NOT_FOUND, "not_found"),
        ];
        for (lease, status, code) in cases {
            let error = AppError::from_lease_mutation(anyhow::Error::from(lease));
            assert_eq!(error.status_and_code(), (status, code));
        }
    }

    #[test]
    fn non_lease_mutation_failures_stay_bad_requests() {
        let error = AppError::from_lease_mutation(anyhow::anyhow!(
            "lease_seconds must be between 15 and 3600"
        ));
        assert_eq!(
            error.status_and_code(),
            (StatusCode::BAD_REQUEST, "bad_request")
        );
    }
}
