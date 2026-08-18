//! HTTP error type for the notifications relay — every status renders the
//! shared JSON envelope from `http-common` (400 with per-field `fields`, 401
//! from the shared JWT middleware, 429 with `Retry-After`). Provider failures
//! do **not** flow through here — they are `200` responses with
//! `success: false`.

use axum::response::{IntoResponse, Response};
pub use http_common::FieldError;

/// A handler error mapped to its HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Request body failed validation (400 with per-field `fields`).
    #[error("invalid request body")]
    InvalidBody(Vec<FieldError>),
    /// Request body was not parseable JSON (400).
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
            AppError::InvalidBody(errors) => http_common::error::invalid_body(&errors),
            AppError::MalformedJson => http_common::error::malformed_json(),
            AppError::RateLimited { retry_after_secs } => {
                http_common::error::rate_limited(retry_after_secs)
            }
        }
    }
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;
