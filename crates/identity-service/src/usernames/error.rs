//! HTTP error type for the usernames surface.
//!
//! Deliberately separate from `http::error::AppError`: the auth handshake
//! still speaks the pre-capture `{error, message}` dialect (its own slice).
//! The legacy status codes are kept (401, 404, 409) but bodies are no longer
//! byte-frozen against the capture (retired 2026-07-23 — no client parses
//! error bodies). Shared shapes (401 trio via `AuthSubject`, 429, the 400
//! problem dialects, the opaque 500) render via `http-common`; the
//! route-specific bare `{"error": "..."}` bodies live here.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_common::problem;
pub use http_common::FieldError;
use serde_json::json;

/// A usernames handler error mapped to the frozen HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum UsernamesError {
    /// Authenticated subject exceeded the per-window limit (429).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets (also the `Retry-After` header).
        retry_after_secs: u64,
    },
    /// Request body failed validation (400 problem details with `errors`).
    #[error("invalid request body")]
    InvalidBody(Vec<FieldError>),
    /// A query parameter failed validation (400 problem details with `errors`).
    #[error("invalid query parameters")]
    InvalidQuery(Vec<FieldError>),
    /// A request header failed validation (400 problem details with `errors`).
    #[error("invalid request header")]
    InvalidHeader(Vec<FieldError>),
    /// Request body was not parseable JSON (400).
    #[error("malformed JSON body")]
    MalformedJson,
    /// The preferred discriminator is already allocated (409).
    #[error("preferred digits taken")]
    PreferredDigitsTaken {
        /// The requested two-digit suffix.
        digits: String,
        /// The base username.
        base: String,
    },
    /// Every discriminator for the base is allocated (409).
    #[error("no digits available")]
    NoDigitsAvailable {
        /// The base username.
        base: String,
    },
    /// The selected `base.digits` was taken concurrently (409).
    #[error("username already taken")]
    UsernameTaken {
        /// The base username.
        base: String,
        /// The selected two-digit suffix.
        digits: String,
    },
    /// The authenticated account has no queued registration (404, new
    /// `/api/v1/registration/queue` surface — no frozen dialect).
    #[error("no queue entry")]
    NoQueueEntry,
    /// The submitted `lifetimePoUDVoucher` cannot be redeemed (400, new
    /// eligibility surface — no frozen dialect; a voucher failure rejects the
    /// claim, it never falls through to another lane).
    #[error("voucher not redeemable")]
    Voucher(crate::eligibility::VoucherError),
    /// The authenticated account has no active payment request (404, new
    /// `/api/v1/usernames/payment-status` surface — no frozen dialect).
    #[error("no active payment request")]
    NoPaymentRequest,
    /// The reservation outbox insert failed (500, captured legacy body).
    #[error("registration persistence failed")]
    PersistenceFailed,
    /// Hard-mode DeviceCheck required a usable `Device-Token-iOS` and none
    /// was present (401, legacy body).
    #[error("device token required")]
    DeviceTokenRequired,
    /// Hard-mode DeviceCheck could not reach Apple to resolve the device
    /// (502, legacy body).
    #[error("device check unavailable")]
    DeviceCheckUnavailable,
    /// The free-registration slot could not be marked used at Apple after a
    /// successful gate. An upstream (Apple) write failure, so a 503 rather
    /// than a generic 500 (diverges from legacy's 500 — the mark failure is
    /// an upstream problem the client can retry).
    #[error("device registration failed")]
    DeviceRegistrationFailed,
    /// Unexpected internal failure; logged, surfaced opaquely (500).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for UsernamesError {
    fn into_response(self) -> Response {
        match self {
            UsernamesError::RateLimited { retry_after_secs } => {
                problem::rate_limited(retry_after_secs)
            }
            UsernamesError::InvalidBody(errors) => problem::invalid_body(&errors),
            UsernamesError::InvalidQuery(errors) => problem::invalid_query(&errors),
            UsernamesError::InvalidHeader(errors) => problem::invalid_header(&errors),
            UsernamesError::MalformedJson => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Malformed JSON in request body" })),
            )
                .into_response(),
            UsernamesError::PreferredDigitsTaken { digits, base } => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("Preferred digits {digits} already taken for username {base}")
                })),
            )
                .into_response(),
            UsernamesError::NoDigitsAvailable { base } => (
                StatusCode::CONFLICT,
                Json(json!({ "error": format!("No digits available for username {base}.") })),
            )
                .into_response(),
            UsernamesError::UsernameTaken { base, digits } => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!(
                        "Username {base}.{digits} already taken. Please try different digits."
                    )
                })),
            )
                .into_response(),
            UsernamesError::NoQueueEntry => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "No queue entry found" })),
            )
                .into_response(),
            UsernamesError::Voucher(reason) => {
                use crate::eligibility::VoucherError;
                let message = match reason {
                    VoucherError::Unknown => "Voucher not found",
                    VoucherError::Spent => "Voucher already used",
                    VoucherError::Expired => "Voucher expired",
                };
                (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
            }
            UsernamesError::NoPaymentRequest => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "No active payment request" })),
            )
                .into_response(),
            UsernamesError::PersistenceFailed => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to persist username registration" })),
            )
                .into_response(),
            UsernamesError::DeviceTokenRequired => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "A valid Device-Token-iOS header is required." })),
            )
                .into_response(),
            UsernamesError::DeviceCheckUnavailable => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "iOS DeviceCheck verification failed" })),
            )
                .into_response(),
            UsernamesError::DeviceRegistrationFailed => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "Failed to mark iOS device as registered with Apple DeviceCheck"
                })),
            )
                .into_response(),
            UsernamesError::Internal(err) => problem::internal(&err),
        }
    }
}

/// Map any `sqlx` error to an internal failure.
impl From<sqlx::Error> for UsernamesError {
    fn from(err: sqlx::Error) -> Self {
        UsernamesError::Internal(err.into())
    }
}

/// Usernames handler result alias.
pub type UsernamesResult<T> = Result<T, UsernamesError>;
