//! HTTP error type for the turn service — every dialect is shared, rendered
//! via `http-common`. This route has no domain error bodies of its own: the
//! legacy handler had no catchable failures (credential minting is pure), so
//! anything unexpected there fell to the opaque global-hook 500, which this
//! service has no code path to produce.

use axum::response::{IntoResponse, Response};
use http_common::problem;
pub use http_common::FieldError;

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
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::RateLimited { retry_after_secs } => problem::rate_limited(retry_after_secs),
            AppError::InvalidBody(errors) => problem::invalid_body(&errors),
            AppError::MalformedJson => problem::malformed_json(),
        }
    }
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;
