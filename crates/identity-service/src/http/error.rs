//! HTTP error type for identity-service handlers.
//!
//! Maps domain failures onto the status codes the mobile clients already expect
//! (`400 WRONG_DATA`, `401`, `403 INTEGRITY_FAILED`, `429`), with a small stable
//! JSON body. Internal failures are logged and surfaced as an opaque `500`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;

/// The shared error envelope returned by every non-2xx response.
///
/// `error` is a stable machine code (see [`AppError`]); `message` is a
/// human-readable detail that may change. This type exists so the OpenAPI
/// document can reference a single error schema across all endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Stable machine code, e.g. `WRONG_DATA`, `UNAUTHORIZED`, `CONFLICT`.
    #[schema(example = "WRONG_DATA")]
    pub error: String,
    /// Human-readable detail; safe to log, not intended for programmatic use.
    #[schema(example = "human-readable detail")]
    pub message: String,
}

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
    /// Platform attestation (App Attest / Play Integrity / key attestation)
    /// failed verification under hard enforcement. Distinct from `Unauthorized`
    /// (bad challenge/proof): the client is talking to us fine, the device/app
    /// failed the genuineness check, so it maps to `403 INTEGRITY_FAILED`.
    #[error("{0}")]
    IntegrityFailed(String),
    /// Google's Android attestation revocation list could not be consulted and
    /// no cached snapshot exists — an infrastructure failure, not a device
    /// failure. Maps to `503` so the client retries with a fresh challenge.
    #[error("attestation CRL unavailable")]
    CrlUnavailable,
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
            AppError::IntegrityFailed(_) => (StatusCode::FORBIDDEN, "INTEGRITY_FAILED"),
            AppError::CrlUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "ATTESTATION_CRL_UNAVAILABLE",
            ),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "CONFLICT"),
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
            }
        };
        let message = match &self {
            AppError::BadRequest(m)
            | AppError::Forbidden(m)
            | AppError::IntegrityFailed(m)
            | AppError::Conflict(m) => m.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;

    async fn observe(err: AppError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        (status, serde_json::from_slice(&bytes).expect("json body"))
    }

    #[tokio::test]
    async fn every_variant_maps_to_its_frozen_status_code_and_message() {
        let cases = [
            (
                AppError::RateLimited,
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "rate limited",
            ),
            (
                AppError::bad_request("candidateSignature is malformed"),
                StatusCode::BAD_REQUEST,
                "WRONG_DATA",
                "candidateSignature is malformed",
            ),
            (
                AppError::Unauthorized,
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "unauthorized",
            ),
            (
                AppError::forbidden("who does not match subject"),
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "who does not match subject",
            ),
            (
                AppError::IntegrityFailed("basic integrity not met".into()),
                StatusCode::FORBIDDEN,
                "INTEGRITY_FAILED",
                "basic integrity not met",
            ),
            (
                AppError::CrlUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "ATTESTATION_CRL_UNAVAILABLE",
                "attestation CRL unavailable",
            ),
            (
                AppError::conflict("username already registered"),
                StatusCode::CONFLICT,
                "CONFLICT",
                "username already registered",
            ),
        ];
        for (err, want_status, want_code, want_message) in cases {
            let label = format!("{err:?}");
            let (status, body) = observe(err).await;
            assert_eq!(status, want_status, "{label}");
            assert_eq!(body["error"], want_code, "{label}");
            assert_eq!(body["message"], want_message, "{label}");
        }
    }

    #[tokio::test]
    async fn sqlx_errors_surface_as_internal_500() {
        let (status, body) = observe(AppError::from(sqlx::Error::PoolTimedOut)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "INTERNAL");
    }

    // The module doc promises internal failures are "surfaced as an opaque
    // 500", but `Internal` is `#[error(transparent)]`, so the wrapped error's
    // own text reaches the wire as `message`. Pinned here as-is; making the
    // body actually opaque is a wire change that needs its own decision.
    #[tokio::test]
    async fn internal_message_currently_echoes_the_wrapped_error_text() {
        let (status, body) = observe(AppError::Internal(anyhow::anyhow!("pool exhausted"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "INTERNAL");
        assert_eq!(body["message"], "pool exhausted");
    }
}
