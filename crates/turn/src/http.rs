//! HTTP surface for `turn-api`: the issue route plus the shared axum
//! primitives (state, errors — the rest lives in `http-common`).
//!
//! Route behavior:
//! - `POST /api/v1/turn/issue`: auth (401) → body-parse (a present-but-
//!   unparseable body → 400; an empty body is an empty command) →
//!   body-validate (400 with per-field `fields`; `regionHint` accepts
//!   string/null/absent and is then **ignored** — reserved) → per-subject
//!   rate limit (429) → mint → **201** (not 200).
//! - Unmatched paths and wrong methods: the JSON `404`.
//!
//! Credential shape (the coturn REST-API construction) lives in
//! [`crate::credentials`]; the `servers` array echoes `ICE_SERVERS` verbatim.

pub mod error;
pub mod proof_routes;
pub mod state;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use http_common::error::not_found;
use http_common::{health, layers, AuthSubject};

use self::error::{AppError, AppResult, FieldError};
use self::state::AppState;

/// This service's own paths and nothing else: no health routes, no service-wide
/// fallback, no middleware stack. That is what makes it mergeable with its
/// siblings in the `all-in-one` role — six copies of `/livez`, six fallbacks and
/// six middleware stacks cannot coexist in one router.
///
/// The in-prefix `fallback(not_found)` on the method router stays here: it
/// is this surface's own dialect for a wrong method, and it must win inside
/// `/api/v1/turn` however the router is assembled.
///
/// The proof routes are present only when proof-authorized issuance is enabled.
/// A hex proof is ~1.6 KB, so an 8 KB cap bounds every legitimate proof request.
pub fn router(state: AppState) -> Router {
    let mut api = Router::new().route("/issue", post(issue_credentials).fallback(not_found));
    if state.proof.is_some() {
        api = api.route(
            "/issue-with-proof",
            post(proof_routes::issue_with_proof)
                .layer(axum::extract::DefaultBodyLimit::max(8 * 1024))
                .fallback(not_found),
        );
    }
    Router::new().nest("/api/v1/turn", api).with_state(state)
}

/// Assemble the full standalone service: [`router`] plus health at root, the
/// service-wide JSON 404, and the production middleware stack.
pub fn routes(state: AppState) -> Router {
    let health = health::router("turn", readiness).with_state(state.clone());
    layers::standard_layers(
        Router::new()
            .merge(health)
            .merge(router(state))
            .fallback(not_found),
    )
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

/// Validate the request body: an object whose optional `regionHint` is a
/// string or null. The value itself is ignored (reserved).
fn validate_body(body: &serde_json::Value) -> Result<(), Vec<FieldError>> {
    if !body.is_object() {
        return Err(vec![FieldError {
            message: http_common::error::expected("object", http_common::error::type_name(body)),
            field: "".to_string(),
        }]);
    }
    match body.get("regionHint") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => Ok(()),
        Some(other) => Err(vec![FieldError {
            message: http_common::error::expected("string", http_common::error::type_name(other)),
            field: "regionHint".to_string(),
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
        (status = 400, description = "Body validation failed (with per-field `fields`), or \
`Malformed JSON in request body` when a body is present but not JSON. An empty body is \
accepted — every field is optional.",
         example = json!({
             "error": "The request body contains invalid values.",
             "fields": [{ "field": "regionHint", "message": "expected string, received number" }]
         })),
        (status = 401, description = "Missing/malformed `Authorization` header or failed token \
verification.",
         example = json!({
             "error": "Token verification failed. The token may be expired or malformed."
         })),
        (status = 429, description = "Per-subject rate limit exceeded (with `Retry-After`).",
         example = json!({ "error": "Rate limit exceeded. Please retry after 60 seconds." })),
    )
)]
pub(crate) async fn issue_credentials(
    State(state): State<AppState>,
    auth: AuthSubject,
    body: axum::body::Bytes,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    // The content type is ignored; an empty body is an empty command, since
    // every field is optional. Anything else must parse as JSON.
    if !body.is_empty() {
        let body: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| AppError::MalformedJson)?;
        validate_body(&body).map_err(AppError::InvalidBody)?;
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
        assert_eq!(errors[0].field, "regionHint");
        assert_eq!(errors[0].message, "expected string, received number");
    }

    #[test]
    fn rejects_non_object_bodies_at_the_root_pointer() {
        for (body, received) in [
            (json!([1, 2]), "array"),
            (json!("nope"), "string"),
            (json!(null), "null"),
        ] {
            let errors = validate_body(&body).unwrap_err();
            assert_eq!(errors[0].field, "");
            assert_eq!(
                errors[0].message,
                format!("expected object, received {received}")
            );
        }
    }
}
