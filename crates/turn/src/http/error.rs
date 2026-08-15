//! HTTP error type for the turn service. The frozen issue route's dialects
//! are shared, rendered via `http-common`; the proof routes are a new
//! contract and render standard RFC 9457 problem details of their own.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
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
    /// Proof-route request rejected before verification (400).
    #[error("bad proof request")]
    BadProofRequest(&'static str),
    /// Proof verification failed — not a member, wrong context/message, or
    /// tampered proof (403). Deliberately unspecific.
    #[error("proof rejected")]
    ProofRejected,
    /// Proof verification cannot run right now — no root snapshot yet (503).
    #[error("proof verification unavailable")]
    ProofUnavailable(&'static str),
    /// Every verification slot stayed busy for the bounded wait (503 with
    /// `Retry-After`).
    #[error("proof verification saturated")]
    ProofBusy,
    /// The proof verifier failed internally (500).
    #[error("proof verification failed internally")]
    ProofInternal,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::RateLimited { retry_after_secs } => problem::rate_limited(retry_after_secs),
            AppError::InvalidBody(errors) => problem::invalid_body(&errors),
            AppError::MalformedJson => problem::malformed_json(),
            AppError::BadProofRequest(detail) => problem::bad_request(detail),
            AppError::ProofRejected => problem_response(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "The supplied proof was not accepted.",
            ),
            AppError::ProofUnavailable(detail) => problem_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                detail,
            ),
            AppError::ProofBusy => {
                let mut response = problem_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Service Unavailable",
                    "All verification slots are busy.",
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
                response
            }
            AppError::ProofInternal => problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "Proof verification failed internally.",
            ),
        }
    }
}

/// Minimal RFC 9457 body for the proof routes' new (non-legacy) statuses.
fn problem_response(status: StatusCode, title: &str, detail: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(serde_json::json!({
            "title": title,
            "detail": detail,
            "status": status.as_u16(),
        })),
    )
        .into_response()
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;
