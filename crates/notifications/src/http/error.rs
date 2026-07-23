//! HTTP error type for the notifications relay — every error dialect is shared,
//! rendered via `http-common` (RFC-9457 problem details, matching the legacy
//! `/api/v1/notify` route: 400 problem+json with per-field `errors`, 401 from
//! the shared JWT middleware, 429 problem+json with `Retry-After`). Provider
//! failures do **not** flow through here — they are `200` responses with
//! `success: false`.

use axum::response::{IntoResponse, Response};
use http_common::problem;
pub use http_common::FieldError;

/// A handler error mapped to the frozen HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Request body failed validation (400 problem details with `errors`).
    #[error("invalid request body")]
    InvalidBody(Vec<FieldError>),
    /// Request body was not parseable JSON (400 plain text, the Hono dialect).
    #[error("malformed JSON body")]
    MalformedJson,
    /// Authenticated subject exceeded the notify rate limit (429).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets (also the `Retry-After` header).
        retry_after_secs: u64,
    },
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::InvalidBody(errors) => problem::invalid_body(&errors),
            AppError::MalformedJson => problem::malformed_json(),
            AppError::RateLimited { retry_after_secs } => problem::rate_limited(retry_after_secs),
        }
    }
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;
