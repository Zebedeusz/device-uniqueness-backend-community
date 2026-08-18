//! The shared JSON error envelope.
//!
//! Every service renders failures as one shape:
//!
//! ```json
//! { "error": "The request body contains invalid values.",
//!   "fields": [ { "field": "who", "message": "must not be empty" } ] }
//! ```
//!
//! `error` is a human-readable summary; `fields` is present only when the
//! failure is per-field validation. The content type is always
//! `application/json`, and the status code carries the machine-readable part
//! of the contract.
//!
//! Handler-specific bodies (404/409/422 texts) stay in each service's own
//! error type — their texts are per-route contract — but they use this same
//! envelope.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// One field failure inside a 400 body.
#[derive(Debug, Clone)]
pub struct FieldError {
    /// Human-readable failure, e.g. `Invalid SS58 address format`.
    pub message: String,
    /// Name of the offending field, e.g. `who`. The empty string means the
    /// value as a whole (a non-object body, say) rather than one field.
    pub field: String,
}

/// Render `{"error": …}` with an optional `fields` array.
fn error_body(
    status: StatusCode,
    message: &str,
    fields: &[FieldError],
    extra_headers: &[(header::HeaderName, String)],
) -> Response {
    let mut body = json!({ "error": message });
    if !fields.is_empty() {
        body["fields"] = fields
            .iter()
            .map(|e| json!({ "field": e.field, "message": e.message }))
            .collect::<Vec<_>>()
            .into();
    }
    let mut response = (status, Json(body)).into_response();
    for (name, value) in extra_headers {
        if let Ok(value) = value.parse() {
            response.headers_mut().insert(name.clone(), value);
        }
    }
    response
}

/// A plain `{"error": …}` at any status, with no field detail.
pub fn message(status: StatusCode, message: &str) -> Response {
    error_body(status, message, &[], &[])
}

/// 401: no `Authorization` header at all.
pub fn missing_auth_header() -> Response {
    message(
        StatusCode::UNAUTHORIZED,
        "Missing Authorization header. Include a valid Bearer token.",
    )
}

/// 401: an `Authorization` header that is not `Bearer <token>`.
pub fn invalid_auth_header() -> Response {
    message(
        StatusCode::UNAUTHORIZED,
        "Authorization header must use the Bearer scheme: \"Bearer <token>\".",
    )
}

/// 401: the Bearer token failed verification.
pub fn invalid_token() -> Response {
    message(
        StatusCode::UNAUTHORIZED,
        "Token verification failed. The token may be expired or malformed.",
    )
}

/// 429: the authenticated subject exceeded the per-window limit (with
/// `Retry-After`).
pub fn rate_limited(retry_after_secs: u64) -> Response {
    error_body(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("Rate limit exceeded. Please retry after {retry_after_secs} seconds."),
        &[],
        &[(header::RETRY_AFTER, retry_after_secs.to_string())],
    )
}

/// 400: rejected before any per-field validation ran.
pub fn bad_request(detail: &str) -> Response {
    message(StatusCode::BAD_REQUEST, detail)
}

/// 402: the caller must present (more) proof before the route serves them.
pub fn payment_required(detail: &str) -> Response {
    message(StatusCode::PAYMENT_REQUIRED, detail)
}

/// 400: the request body failed validation.
pub fn invalid_body(fields: &[FieldError]) -> Response {
    error_body(
        StatusCode::BAD_REQUEST,
        "The request body contains invalid values.",
        fields,
        &[],
    )
}

/// 400: a query parameter failed validation.
pub fn invalid_query(fields: &[FieldError]) -> Response {
    error_body(
        StatusCode::BAD_REQUEST,
        "The request query contains invalid values.",
        fields,
        &[],
    )
}

/// 400: a path parameter failed validation.
pub fn invalid_param(fields: &[FieldError]) -> Response {
    error_body(
        StatusCode::BAD_REQUEST,
        "The request path contains invalid values.",
        fields,
        &[],
    )
}

/// 400: a request header failed validation.
pub fn invalid_header(fields: &[FieldError]) -> Response {
    error_body(
        StatusCode::BAD_REQUEST,
        "The request headers contain invalid values.",
        fields,
        &[],
    )
}

/// 400: the body was not parseable JSON.
pub fn malformed_json() -> Response {
    message(StatusCode::BAD_REQUEST, "Malformed JSON in request body")
}

/// 500: unexpected internal failure; logged here, surfaced opaquely.
pub fn internal(err: &anyhow::Error) -> Response {
    tracing::error!(error = ?err, "internal error");
    message(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal server error. Please try again.",
    )
}

/// The 404 fallback, usable directly as an axum fallback handler (unmatched
/// paths; a matched path with the wrong method still gets axum's 405).
pub async fn not_found() -> Response {
    message(StatusCode::NOT_FOUND, "Not found")
}

/// The JSON type name of a value, for `expected …, received …` messages.
pub fn type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The received-type name for an optional field (`nothing` when absent).
pub fn received_name(value: Option<&serde_json::Value>) -> &'static str {
    value.map_or("nothing", type_name)
}

/// A type-mismatch message: `expected string, received number`.
pub fn expected(expected: &str, received: &str) -> String {
    format!("expected {expected}, received {received}")
}

/// The message for a string that must carry at least one character.
pub const MUST_NOT_BE_EMPTY: &str = "must not be empty";

/// The message for a number that must be greater than zero.
pub const MUST_BE_POSITIVE: &str = "must be greater than 0";

/// The message for an array that exceeded its item bound.
pub fn at_most_items(max_items: usize) -> String {
    format!("must contain at most {max_items} items")
}

/// The message for a string that failed its pattern, e.g.
/// `must match ^([a-z]{6,})$`.
pub fn must_match(pattern: &str) -> String {
    format!("must match {pattern}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names_describe_the_json_type() {
        assert_eq!(type_name(&json!(null)), "null");
        assert_eq!(type_name(&json!(5)), "number");
        assert_eq!(type_name(&json!([1])), "array");
        assert_eq!(received_name(None), "nothing");
        assert_eq!(
            expected("string", received_name(Some(&json!(5)))),
            "expected string, received number"
        );
    }

    #[tokio::test]
    async fn a_field_failure_carries_the_fields_array() {
        let response = invalid_body(&[FieldError {
            message: MUST_NOT_BE_EMPTY.to_string(),
            field: "who".to_string(),
        }]);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
            json!({
                "error": "The request body contains invalid values.",
                "fields": [{ "field": "who", "message": "must not be empty" }],
            })
        );
    }

    #[tokio::test]
    async fn a_plain_failure_omits_the_fields_array() {
        let response = malformed_json();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
            json!({ "error": "Malformed JSON in request body" })
        );
    }
}
