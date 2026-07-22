//! HTTP error type reproducing the legacy invitation-ticket wire contract.
//!
//! The shared dialects (401 trio / 429 / 400 problem details / Hono text 400
//! / opaque 500) render via `http-common`; only this route's own bare
//! `{"error": "..."}` bodies (409 `Ticket race lost` / 422 `Pool exhausted`)
//! live here.

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
    /// No `available` ticket in the pool at the pre-check (422).
    #[error("pool exhausted")]
    PoolExhausted,
    /// The pre-check saw tickets but the claim transaction found none (409).
    #[error("ticket race lost")]
    TicketRaceLost,
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
            AppError::PoolExhausted => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "Pool exhausted" })),
            )
                .into_response(),
            AppError::TicketRaceLost => (
                StatusCode::CONFLICT,
                Json(json!({ "error": "Ticket race lost" })),
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
