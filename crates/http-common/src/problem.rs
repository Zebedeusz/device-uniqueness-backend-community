//! The legacy error dialects, byte-for-byte.
//!
//! The legacy backend spoke three shapes on the migrated routes and all are
//! frozen (golden-captured in each service's `tests/fixtures/`):
//! - **RFC 9457 problem details** (`application/problem+json`) from the JWT
//!   middleware (401), the rate limiter (429), and body validation (400).
//! - **Plain text** from Hono itself: `Malformed JSON in request body` (400,
//!   un-spaced `text/plain;charset=UTF-8`) and the default `404 Not Found`
//!   (spaced `text/plain; charset=UTF-8`).
//! - **The opaque global-hook 500**: the literal
//!   `{"error":"Oops! Something went wrong."}`.
//!
//! Handler-specific bare `{"error": "..."}` bodies (404/409/422) stay in each
//! service's own error type — their texts are per-route contract.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Base URL of the problem-type registry the legacy bodies point at.
const PROBLEM_REGISTRY: &str = "https://problems-registry.smartbear.com";

/// One field failure inside a 400 problem-details body.
#[derive(Debug, Clone)]
pub struct FieldError {
    /// Human-readable failure, e.g. `Invalid SS58 address format`.
    pub detail: String,
    /// JSON pointer to the offending field, e.g. `#/who`.
    pub pointer: String,
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

/// 401: no `Authorization` header at all.
pub fn missing_auth_header() -> Response {
    problem(
        StatusCode::UNAUTHORIZED,
        json!({
            "type": format!("{PROBLEM_REGISTRY}/missing-request-header"),
            "title": "Missing Authorization Header",
            "detail": "Write requests with an Authorization header must include a valid Bearer token.",
            "status": 401,
        }),
        &[],
    )
}

/// 401: an `Authorization` header that is not `Bearer <token>`.
pub fn invalid_auth_header() -> Response {
    problem(
        StatusCode::UNAUTHORIZED,
        json!({
            "type": format!("{PROBLEM_REGISTRY}/invalid-request-header-format"),
            "title": "Invalid Authorization Header",
            "detail": "Authorization header must use Bearer scheme: \"Bearer <token>\".",
            "status": 401,
        }),
        &[],
    )
}

/// 401: the Bearer token failed verification.
pub fn invalid_token() -> Response {
    problem(
        StatusCode::UNAUTHORIZED,
        json!({
            "type": format!("{PROBLEM_REGISTRY}/unauthorized"),
            "title": "Invalid Token",
            "detail": "Token verification failed. The token may be expired or malformed.",
            "status": 401,
        }),
        &[],
    )
}

/// 429: the authenticated subject exceeded the per-window limit (with
/// `Retry-After`).
pub fn rate_limited(retry_after_secs: u64) -> Response {
    problem(
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
    )
}

/// 400: the request body failed validation (problem details with `errors`).
pub fn invalid_body(errors: &[FieldError]) -> Response {
    problem(
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
    )
}

/// 400: the body was not parseable JSON — legacy exact bytes, including the
/// un-spaced content type Hono emits for `c.text(...)`.
pub fn malformed_json() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "text/plain;charset=UTF-8")],
        "Malformed JSON in request body",
    )
        .into_response()
}

/// 500: unexpected internal failure; logged here, surfaced as the legacy
/// global error hook's literal body.
pub fn internal(err: &anyhow::Error) -> Response {
    tracing::error!(error = ?err, "internal error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "Oops! Something went wrong." })),
    )
        .into_response()
}

/// The legacy (Hono) default not-found response, byte-for-byte. Usable
/// directly as an axum fallback handler (unmatched paths AND wrong methods —
/// Hono routes are method-specific, so a wrong method was a 404, never a 405).
pub async fn hono_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=UTF-8")],
        "404 Not Found",
    )
        .into_response()
}
