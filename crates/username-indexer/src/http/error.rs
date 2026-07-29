//! HTTP error type reproducing the legacy usernames wire contract
//! (golden-captured in `tests/fixtures/legacy-wire.json`).
//!
//! The shared dialects (query problem details, 429, opaque 500) render via
//! `http-common`; only this route's own bare `{"error": "..."}` body (the
//! cursor 400) lives here.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_common::problem;
pub use http_common::FieldError;
use serde_json::json;

use crate::search::SearchError;

/// A handler error mapped to the frozen HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A query parameter failed validation (400 problem details with `errors`).
    #[error("invalid query parameters")]
    InvalidQuery(Vec<FieldError>),
    /// The continuation cursor failed decoding (400, bare legacy body).
    #[error("invalid cursor")]
    InvalidCursor,
    /// Client exceeded the public per-IP limit (429).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets (also the `Retry-After` header).
        retry_after_secs: u64,
    },
    /// The proof-of-compute gate refused the request (400 malformed / 402 rest).
    #[error(transparent)]
    Poc(#[from] crate::poc::Rejection),
    /// Unexpected internal failure; logged, surfaced opaquely (500).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::InvalidQuery(errors) => problem::invalid_query(&errors),
            AppError::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid cursor" })),
            )
                .into_response(),
            AppError::RateLimited { retry_after_secs } => problem::rate_limited(retry_after_secs),
            AppError::Poc(rejection) => match rejection {
                crate::poc::Rejection::Malformed => problem::bad_request(rejection.detail()),
                other => problem::payment_required(other.detail()),
            },
            AppError::Internal(err) => problem::internal(&err),
        }
    }
}

impl From<SearchError> for AppError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::InvalidQuery(errors) => Self::InvalidQuery(errors),
            SearchError::InvalidCursor => Self::InvalidCursor,
            SearchError::Database(error) => Self::Internal(error.into()),
        }
    }
}

/// Map any `sqlx` error to an internal failure.
impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Internal(err.into())
    }
}

/// Result returned by HTTP handlers.
pub type AppResult<T> = Result<T, AppError>;
