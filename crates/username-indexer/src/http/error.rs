//! HTTP error type for the usernames read surface.
//!
//! The shared statuses (the query 400, 429, the opaque 500) render via
//! `http-common`; only this route's own message (the cursor 400) lives here.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
pub use http_common::FieldError;
use serde_json::json;

use crate::search::SearchError;

/// A handler error mapped to its HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A query parameter failed validation (400 with per-field `fields`).
    #[error("invalid query parameters")]
    InvalidQuery(Vec<FieldError>),
    /// The continuation cursor failed decoding (400).
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
            AppError::InvalidQuery(errors) => http_common::error::invalid_query(&errors),
            AppError::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid cursor" })),
            )
                .into_response(),
            AppError::RateLimited { retry_after_secs } => {
                http_common::error::rate_limited(retry_after_secs)
            }
            AppError::Poc(rejection) => match rejection {
                crate::poc::Rejection::Malformed => {
                    http_common::error::bad_request(rejection.detail())
                }
                other => http_common::error::payment_required(other.detail()),
            },
            AppError::Internal(err) => http_common::error::internal(&err),
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
