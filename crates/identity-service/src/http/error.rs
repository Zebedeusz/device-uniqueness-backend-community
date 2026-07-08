//! HTTP error type for identity-service handlers.
//!
//! Maps domain failures onto the status codes the mobile clients already expect
//! (`400 WRONG_DATA`, `401`, `403 INTEGRITY_FAILED`, `429`), with a small stable
//! JSON body. Internal failures are logged and surfaced as an opaque `500`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// A handler error mapped to an HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Client exceeded the per-route rate limit.
    #[error("rate limited")]
    RateLimited,
    /// Malformed request data (missing/invalid header or body field).
    #[error("{0}")]
    BadRequest(String),
    /// Challenge or client proof failed verification.
    #[error("unauthorized")]
    Unauthorized,
    /// Authenticated, but not allowed to perform the action (e.g. `who` != subject).
    #[error("{0}")]
    Forbidden(String),
    /// Request conflicts with existing state (e.g. username/digits already taken).
    #[error("{0}")]
    Conflict(String),
    /// Unexpected internal failure; logged, surfaced opaquely.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// Convenience for a `400` from any displayable message.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    /// Convenience for a `409` from any displayable message.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// Convenience for a `403` from any displayable message.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "WRONG_DATA"),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED"),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "FORBIDDEN"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "CONFLICT"),
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
            }
        };
        let message = match &self {
            AppError::BadRequest(m) | AppError::Forbidden(m) | AppError::Conflict(m) => m.clone(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;

/// Map any `sqlx` error to an internal failure.
impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Internal(err.into())
    }
}
