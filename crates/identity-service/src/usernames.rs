//! `/api/v1/attester` and the identity-owned username API surface
//! (availability reads and registration intake).
//!
//! The usernames routes keep the legacy status codes (401 via `AuthSubject`,
//! the 404 fallbacks, the 409 conflicts) but no longer reproduce the legacy
//! error bodies byte-for-byte — no client parses them (retired 2026-07-23;
//! the captures remain as evidence in `tests/fixtures/legacy-wire.json`).
//! Validation failures are normal problem-details 400s, and the captured
//! intentional 500s (missing content type, regex-failed candidateSignature)
//! are normal 400s now. The legacy CORS middleware bound only the collection
//! root, so `/` answers preflights and stamps
//! `access-control-allow-origin: *` on every response, while `/available`
//! carries no CORS at all — exactly as captured.

pub mod available;
pub mod error;
pub mod register;

use std::collections::BTreeSet;

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::header::{HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_common::problem::hono_not_found;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::http::state::AppState;
use axum::extract::State;

use self::error::{UsernamesError, UsernamesResult};
use crate::chain::outbox;

/// Minimum base-username length.
const MIN_BASE_LEN: usize = 6;

/// Maximum base-username length on the registration path (the legacy
/// `MAX_USERNAME_LENGTH - N_USERNAME_DIGITS - 1`; availability has no cap).
pub(crate) const MAX_BASE_LEN: usize = 29;

/// Base-username rules, shared by the read and write paths so they can't
/// disagree: length >= 6, lowercase ASCII letters.
fn is_valid_base(base: &str) -> bool {
    base.len() >= MIN_BASE_LEN && base.bytes().all(|b| b.is_ascii_lowercase())
}

/// The free discriminators for a base: `1..=99` minus the taken set (`00` is
/// never offered). Shared so availability and registration agree on what's free.
fn available_digits(taken: &BTreeSet<u8>) -> Vec<u8> {
    (1..=99u8).filter(|d| !taken.contains(d)).collect()
}

fn merge_discriminators(mut chain: BTreeSet<u8>, outbox: BTreeSet<u8>) -> BTreeSet<u8> {
    chain.extend(outbox);
    chain
}

/// Allocations visible either on People Chain or in the durable reservation
/// outbox. Both reads run concurrently; their union is the availability and
/// registration source of truth while a reservation is waiting for the writer.
pub(crate) async fn taken_discriminators(
    state: &AppState,
    base: &str,
) -> UsernamesResult<BTreeSet<u8>> {
    let (chain, pending) = tokio::try_join!(
        async {
            state
                .chain
                .taken_discriminators(base)
                .await
                .map_err(UsernamesError::from)
        },
        async {
            outbox::allocated_discriminators(&state.pool, base)
                .await
                .map_err(UsernamesError::from)
        },
    )?;
    Ok(merge_discriminators(chain, pending))
}

/// Subject-keyed rate limit for the authenticated usernames surface (never
/// IP — the CGNAT postmortem). Renders the frozen 429 with `Retry-After`.
pub(crate) fn check_rate_limit(state: &AppState, subject: &str) -> UsernamesResult<()> {
    let key = format!("/api/v1/usernames:{subject}");
    if state.limiter.allow(&key) {
        return Ok(());
    }
    Err(UsernamesError::RateLimited {
        retry_after_secs: state.config.auth_rate_window.as_secs(),
    })
}

/// Parse the request body as JSON, content type ignored (the legacy
/// content-type quirks — available's opaque 500, register's silent
/// empty-object substitution — were retired 2026-07-23). Unparseable → 400.
pub(crate) fn parse_json_body(body: &Bytes) -> UsernamesResult<Value> {
    serde_json::from_slice(body).map_err(|_| UsernamesError::MalformedJson)
}

/// Build the `/usernames` router.
///
/// Method fallbacks keep wrong-method requests on the Hono 404 dialect. The
/// collection root additionally answers CORS preflights and stamps
/// `access-control-allow-origin: *` on every response (captured legacy
/// `cors()` behavior; the response ETag legacy also emitted there is a
/// recorded divergence — an entity tag on a POST response has no caching
/// semantics).
pub fn router() -> Router<AppState> {
    let root = Router::new()
        .route(
            "/",
            post(register::register)
                .options(collection_preflight)
                .fallback(hono_not_found),
        )
        .layer(axum::middleware::from_fn(allow_any_origin));
    root.route(
        "/available",
        post(available::check).fallback(hono_not_found),
    )
}

/// Stamp `access-control-allow-origin: *` on every collection-root response,
/// Origin header or not — the captured hono `cors()` behavior.
async fn allow_any_origin(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// The captured collection-root preflight: `204` with the hono `cors()`
/// defaults (the allow-origin header is stamped by [`allow_any_origin`]).
async fn collection_preflight(headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let out = response.headers_mut();
    out.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,HEAD,PUT,POST,DELETE,PATCH"),
    );
    if let Some(requested) = headers.get(axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS) {
        out.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
            requested.clone(),
        );
    }
    out.insert(
        axum::http::header::VARY,
        HeaderValue::from_static("Access-Control-Request-Headers"),
    );
    response
}

/// `{ attester }` — the on-chain attester authority public key.
#[derive(Serialize, ToSchema)]
pub struct AttesterResponse {
    /// `0x`-hex sr25519 public key the backend writes registrations as.
    #[schema(example = "0xe4cd20d6d1e0e119d63a943afd2d7496fbbb0ac8e7cd99c3c5f16b63a68e7432")]
    attester: String,
}

/// `GET /api/v1/attester` — the attester authority public key (`0x`+hex, public).
#[utoipa::path(
    get,
    path = "/api/v1/attester",
    tag = "Discovery",
    responses(
        (status = 200, description = "The on-chain attester account the backend writes as.", body = AttesterResponse,
         example = json!({ "attester": "0xe4cd20d6d1e0e119d63a943afd2d7496fbbb0ac8e7cd99c3c5f16b63a68e7432" }))
    )
)]
pub async fn attester(State(state): State<AppState>) -> Json<AttesterResponse> {
    Json(AttesterResponse {
        attester: state.config.attester_public_key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attester_response_keeps_the_mobile_wire_shape() {
        let response = AttesterResponse {
            attester: "0x86aac84d".to_string(),
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "attester": "0x86aac84d" })
        );
    }

    #[test]
    fn base_validation_shared_by_read_and_write_paths() {
        assert!(is_valid_base("alicexyz"));
        assert!(is_valid_base(&"a".repeat(30))); // availability has no max
        assert!(!is_valid_base("short"));
        assert!(!is_valid_base("Alicexyz"));
        assert!(!is_valid_base("alice123"));
    }

    #[test]
    fn available_excludes_taken_and_zero() {
        let taken: BTreeSet<u8> = [1u8, 2, 99].into_iter().collect();
        let available = available_digits(&taken);
        assert!(!available.contains(&0));
        assert!(!available.contains(&1));
        assert!(!available.contains(&99));
        assert!(available.contains(&3));
        assert_eq!(available.len(), 99 - 3);
    }

    #[test]
    fn allocation_union_keeps_chain_and_outbox_digits() {
        let chain: BTreeSet<u8> = [1, 2].into_iter().collect();
        let outbox: BTreeSet<u8> = [2, 3].into_iter().collect();
        assert_eq!(
            merge_discriminators(chain, outbox),
            [1, 2, 3].into_iter().collect()
        );
    }

    #[test]
    fn body_parses_as_json_regardless_of_content_type() {
        assert!(parse_json_body(&Bytes::from_static(b"{\"usernames\":[]}")).is_ok());
        assert!(matches!(
            parse_json_body(&Bytes::from_static(b"{not json")),
            Err(UsernamesError::MalformedJson)
        ));
        assert!(matches!(
            parse_json_body(&Bytes::new()),
            Err(UsernamesError::MalformedJson)
        ));
    }
}
