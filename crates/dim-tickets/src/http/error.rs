//! HTTP error type reproducing the legacy dim-ticket wire contract.
//!
//! The legacy backend speaks two error dialects on these routes and both are
//! frozen:
//! - **RFC 9457 problem details** (`application/problem+json`) from the JWT
//!   middleware (401), the rate limiter (429), and body validation (400).
//! - **Bare `{"error": "..."}` bodies** from the route handlers themselves
//!   (404 / 409 / 422) and the global error hook (500).

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Base URL of the problem-type registry the legacy bodies point at.
const PROBLEM_REGISTRY: &str = "https://problems-registry.smartbear.com";

/// One field failure inside a 400 problem-details body.
#[derive(Debug, Clone)]
pub struct FieldError {
    /// Human-readable failure, e.g. `Required`.
    pub detail: String,
    /// JSON pointer to the offending field, e.g. `#/who`.
    pub pointer: String,
}

/// A handler error mapped to the frozen HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// No `Authorization` header at all (401 problem details).
    #[error("missing authorization header")]
    MissingAuthHeader,
    /// An `Authorization` header that is not `Bearer <token>` (401).
    #[error("invalid authorization header")]
    InvalidAuthHeader,
    /// Bearer token failed verification (401).
    #[error("invalid token")]
    InvalidToken,
    /// Authenticated subject exceeded the per-window limit (429).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets (also the `Retry-After` header).
        retry_after_secs: u64,
    },
    /// Request body failed validation (400 problem details with `errors`).
    #[error("invalid request body")]
    InvalidBody(Vec<FieldError>),
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

/// Respond with `application/problem+json` (the legacy middleware dialect).
fn problem(
    status: StatusCode,
    body: serde_json::Value,
    extra_headers: &[(header::HeaderName, String)],
) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/problem+json".to_string())],
        Json(body),
    )
        .into_response();
    for (name, value) in extra_headers {
        if let Ok(value) = value.parse() {
            response.headers_mut().insert(name.clone(), value);
        }
    }
    response
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::MissingAuthHeader => problem(
                StatusCode::UNAUTHORIZED,
                json!({
                    "type": format!("{PROBLEM_REGISTRY}/missing-request-header"),
                    "title": "Missing Authorization Header",
                    "detail": "Write requests with an Authorization header must include a valid Bearer token.",
                    "status": 401,
                }),
                &[],
            ),
            AppError::InvalidAuthHeader => problem(
                StatusCode::UNAUTHORIZED,
                json!({
                    "type": format!("{PROBLEM_REGISTRY}/invalid-request-header-format"),
                    "title": "Invalid Authorization Header",
                    "detail": "Authorization header must use Bearer scheme: \"Bearer <token>\".",
                    "status": 401,
                }),
                &[],
            ),
            AppError::InvalidToken => problem(
                StatusCode::UNAUTHORIZED,
                json!({
                    "type": format!("{PROBLEM_REGISTRY}/unauthorized"),
                    "title": "Invalid Token",
                    "detail": "Token verification failed. The token may be expired or malformed.",
                    "status": 401,
                }),
                &[],
            ),
            AppError::RateLimited { retry_after_secs } => problem(
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "type": format!("{PROBLEM_REGISTRY}/too-many-requests"),
                    "title": "Too Many Requests",
                    "detail": format!(
                        "Rate limit exceeded. Please retry after {retry_after_secs} seconds."
                    ),
                    "status": 429,
                }),
                &[(header::RETRY_AFTER, retry_after_secs.to_string())],
            ),
            AppError::InvalidBody(errors) => problem(
                StatusCode::BAD_REQUEST,
                json!({
                    "type": format!("{PROBLEM_REGISTRY}/invalid-body-property-value"),
                    "title": "Invalid Body Property Value",
                    "detail": "The request body contains an invalid body property value.",
                    "status": 400,
                    "errors": errors
                        .iter()
                        .map(|e| json!({ "detail": e.detail, "pointer": e.pointer }))
                        .collect::<Vec<_>>(),
                }),
                &[],
            ),
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
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "Internal Server Error" })),
                )
                    .into_response()
            }
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
