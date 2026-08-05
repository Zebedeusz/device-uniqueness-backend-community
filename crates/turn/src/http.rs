//! HTTP surface for `turn-api`: the frozen issue route plus the shared axum
//! primitives (state, errors — the rest lives in `http-common`).
//!
//! Route behavior — including its quirks — is the legacy contract, golden-
//! captured from the real TypeScript route (`tests/fixtures/legacy-wire.json`):
//! - `POST /api/v1/turn/issue`: auth (401, the shared middleware ran first in
//!   legacy too) → body-parse (unparseable/empty JSON → the Hono plain-text
//!   400; **missing JSON content-type → 201**, because legacy skipped
//!   validation and the handler optional-chained the absent body — unlike
//!   dim/invite, where the same skip crashed into a 500) → body-validate
//!   (400 problem details carrying zod v4 messages; `regionHint` accepts
//!   string/null/absent and is then **ignored** — reserved) → per-subject
//!   rate limit (429) → mint → **201** (not 200).
//! - Unmatched paths and wrong methods: the Hono default `404 Not Found`
//!   plain-text body.
//!
//! Credential shape (the coturn REST-API construction) lives in
//! [`crate::credentials`]; the `servers` array echoes `ICE_SERVERS` verbatim.

pub mod error;
pub mod state;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use http_common::problem::hono_not_found;
use http_common::{health, layers, zod, AuthSubject};

use self::error::{AppError, AppResult, FieldError};
use self::state::AppState;

/// Assemble the full router: health at root, the frozen `/api/v1/turn`
/// surface below, wrapped in the production middleware stack.
pub fn routes(state: AppState) -> Router {
    let api = Router::new().route("/issue", post(issue_credentials).fallback(hono_not_found));

    let router = Router::new()
        .merge(health::router("turn", readiness))
        .nest("/api/v1/turn", api)
        .fallback(hono_not_found)
        .with_state(state);
    layers::standard_layers(router)
}

/// Stateless service: nothing to gate readiness on — if the process serves,
/// it can mint.
pub async fn readiness(_state: AppState) -> health::Readiness {
    Ok(&[])
}

/// Enforce the per-subject fixed window; `429` with `Retry-After` on excess.
fn check_rate_limit(state: &AppState, subject: &str) -> Result<(), AppError> {
    if state.limiter.allow(subject) {
        Ok(())
    } else {
        Err(AppError::RateLimited {
            retry_after_secs: state.limiter.window_secs(),
        })
    }
}

/// Validate the request body against the frozen schema: an object whose
/// optional `regionHint` is a string or null, reproducing the legacy zod v4
/// messages and pointers. The value itself is ignored (reserved).
fn validate_body(body: &serde_json::Value) -> Result<(), Vec<FieldError>> {
    if !body.is_object() {
        return Err(vec![FieldError {
            detail: zod::invalid_input("object", zod::type_name(body)),
            pointer: "#/".to_string(),
        }]);
    }
    match body.get("regionHint") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => Ok(()),
        Some(other) => Err(vec![FieldError {
            detail: zod::invalid_input("string", zod::type_name(other)),
            pointer: "#/regionHint".to_string(),
        }]),
    }
}

/// Issue short-lived TURN credentials for WebRTC ICE negotiation (JWT-gated).
#[utoipa::path(
    post,
    path = "/api/v1/turn/issue",
    tag = "TURN",
    security(("bearer_jwt" = [])),
    request_body = crate::openapi::IssueRequest,
    responses(
        (status = 201, description = "Credentials minted: `username` is \
`{unixExpiry}:{hexId}` (expiry = now + TTL), `password` is the base64 HMAC over `username` \
under the secret shared with the TURN relay (the coturn REST-API construction), `servers` \
echoes the configured ICE server list.",
         body = crate::openapi::IssueResponse),
        (status = 400, description = "Body validation failed (RFC 9457 problem details with \
per-field `errors`); or plain-text `Malformed JSON in request body` when the body is present \
but not JSON. A request with no JSON content-type skips validation entirely and succeeds \
(legacy quirk).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-body-property-value",
             "title": "Invalid Body Property Value",
             "detail": "The request body contains an invalid body property value.",
             "status": 400,
             "errors": [{ "detail": "Invalid input: expected string, received number", "pointer": "#/regionHint" }]
         })),
        (status = 401, description = "Missing/malformed `Authorization` header or failed token \
verification (RFC 9457 problem details).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/unauthorized",
             "title": "Invalid Token",
             "detail": "Token verification failed. The token may be expired or malformed.",
             "status": 401
         })),
        (status = 429, description = "Per-subject rate limit exceeded (RFC 9457 problem \
details, with `Retry-After`).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/too-many-requests",
             "title": "Too Many Requests",
             "detail": "Rate limit exceeded. Please retry after 60 seconds.",
             "status": 429
         })),
    )
)]
pub(crate) async fn issue_credentials(
    State(state): State<AppState>,
    auth: AuthSubject,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    match body {
        // Legacy quirk: without a JSON content-type, Hono skipped validation
        // and the handler tolerated the absent body — a 201, not an error.
        Err(axum::extract::rejection::JsonRejection::MissingJsonContentType(_)) => {}
        // A present-but-unparseable body is the Hono plain-text 400.
        Err(_) => return Err(AppError::MalformedJson),
        Ok(Json(body)) => validate_body(&body).map_err(AppError::InvalidBody)?,
    }

    check_rate_limit(&state, &auth.subject)?;

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let mut id = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let credentials = state.issuer.issue(now_unix, id);

    tracing::info!(ttl_secs = state.config.ttl_secs, "TURN credentials issued");
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "servers": state.config.ice_servers,
            "username": credentials.username,
            "password": credentials.password,
            "ttl": state.config.ttl_secs,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_absent_null_and_string_region_hints() {
        assert!(validate_body(&json!({})).is_ok());
        assert!(validate_body(&json!({ "regionHint": null })).is_ok());
        assert!(validate_body(&json!({ "regionHint": "eu-west" })).is_ok());
    }

    #[test]
    fn rejects_non_string_region_hints() {
        let errors = validate_body(&json!({ "regionHint": 5 })).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "#/regionHint");
        assert_eq!(
            errors[0].detail,
            "Invalid input: expected string, received number"
        );
    }

    #[test]
    fn rejects_non_object_bodies_at_the_root_pointer() {
        for (body, received) in [
            (json!([1, 2]), "array"),
            (json!("nope"), "string"),
            (json!(null), "null"),
        ] {
            let errors = validate_body(&body).unwrap_err();
            assert_eq!(errors[0].pointer, "#/");
            assert_eq!(
                errors[0].detail,
                format!("Invalid input: expected object, received {received}")
            );
        }
    }
}
