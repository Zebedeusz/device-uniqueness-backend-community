//! HTTP error type reproducing the legacy dim-ticket wire contract
//! (golden-captured in `tests/fixtures/legacy-wire.json`).
//!
//! The shared dialects (401 trio / 429 / 400 problem details / Hono text 400
//! / opaque `Oops! Something went wrong.` 500) render via `http-common`; only
//! this route's own bare `{"error": "..."}` bodies (404 / 409 / 422) live
//! here.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_common::problem;
pub use http_common::FieldError;
use serde_json::json;

/// A handler error mapped to the frozen HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Authenticated subject exceeded the per-window limit (429).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets (also the `Retry-After` header).
        retry_after_secs: u64,
    },
    /// Request body failed validation (400 problem details with `errors`).
    #[error("invalid request body")]
    InvalidBody(Vec<FieldError>),
    /// Request body was not parseable JSON (400 plain text, the Hono dialect).
    #[error("malformed JSON body")]
    MalformedJson,
    /// No ticket row for the requested address (404).
    #[error("ticket not found")]
    TicketNotFound,
    /// The address already holds a ticket (409).
    #[error("ticket already exists")]
    TicketAlreadyExists,
    /// The inviter's on-chain quota is exhausted (422).
    #[error("no available invites")]
    NoAvailableInvites,
    /// Unexpected internal failure; logged, surfaced opaquely (500).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::RateLimited { retry_after_secs } => problem::rate_limited(retry_after_secs),
            AppError::InvalidBody(errors) => problem::invalid_body(&errors),
            AppError::MalformedJson => problem::malformed_json(),
            AppError::TicketNotFound => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Ticket not found" })),
            )
                .into_response(),
            AppError::TicketAlreadyExists => (
                StatusCode::CONFLICT,
                Json(json!({ "error": "Ticket already exists" })),
            )
                .into_response(),
            AppError::NoAvailableInvites => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "No available invites" })),
            )
                .into_response(),
            AppError::Internal(err) => problem::internal(&err),
        }
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
