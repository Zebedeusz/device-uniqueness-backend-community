//! Axum router, state, errors, and health endpoints for username-indexer.

pub mod error;
pub mod health;
pub mod middleware;
pub mod state;

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_common::layers;
use http_common::problem::hono_not_found;

use self::error::AppResult;
pub use self::state::AppState;
use crate::poc::Puzzle;
use crate::search::{SearchQuery, SearchResponse};

/// Assemble health endpoints and the public username read API.
///
/// The rate limiter wraps the public search route (health endpoints stay
/// unthrottled), and the proof-of-compute gate sits inside it: a caller with a
/// valid JWT or a solved puzzle is served, anyone else gets the gate's `402`.
/// With `POC_ENABLED=false` (the default) the gate is a pass-through and the
/// issuance route is not mounted at all.
///
/// Unmatched paths and methods fall back to the frozen Hono `404 Not Found` —
/// which is also what serves the two deliberately-dropped legacy endpoints: the
/// list (`GET /api/v1/usernames`) and the single-username lookup
/// (`GET /api/v1/usernames/{username}`, deprecated in favour of paginated
/// `search`).
pub fn routes(state: AppState) -> Router {
    // Layer order: the later `route_layer` is the outer one, so the cheap per-IP
    // rate limit runs before any signature or HMAC work.
    //
    // Both layers wrap the GET handler only — NOT the method fallback. Layering
    // the whole method router would make a wrong-method request answer the
    // gate's 402 instead of the frozen Hono 404, so `fallback` is attached
    // afterwards and stays outside them.
    let search = get(search_usernames)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::poc_gate,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit,
        ))
        .fallback(hono_not_found);
    let api = Router::new().route("/api/v1/usernames/search", search);
    let poc_api = if state.poc.is_some() {
        Router::new().route(
            "/api/v1/poc/issue",
            post(issue_puzzle).fallback(hono_not_found),
        )
    } else {
        Router::new()
    };
    let router = Router::new()
        .merge(health::router())
        .merge(api)
        .merge(poc_api)
        .fallback(hono_not_found)
        .with_state(state);
    layers::standard_layers(router)
}

/// Issue a proof-of-compute puzzle for an unauthenticated caller.
#[utoipa::path(
    post,
    path = "/api/v1/poc/issue",
    tag = "Proof of compute",
    responses(
        (status = 201, description = "A fresh puzzle to solve. Mine a `counter` whose \
            `sha256(sessionId || timestamp || counter)` has at least `difficulty` leading zero \
            bits, then send `Proof-Of-Compute: base64(sessionId:timestamp:difficulty:counter:checksum)` \
            on the search request. Callers holding an identity-service JWT do not need a puzzle.",
         body = Puzzle,
         example = json!({
             "sessionId": "1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed",
             "timestamp": 1700000000000i64,
             "difficulty": 16,
             "checksum": "c8828951fd6c123fdbf6501f111d27dd3f260839344a7370e0dd8f20e2c40482"
         })),
        (status = 404, description = "Proof of compute is disabled on this deployment \
            (`POC_ENABLED=false`): the route is not mounted and the service-wide plain-text 404 \
            answers instead.",
         body = String)
    )
)]
pub(crate) async fn issue_puzzle(
    State(state): State<AppState>,
) -> (axum::http::StatusCode, Json<Puzzle>) {
    let puzzle = state
        .poc
        .as_ref()
        .expect("the issuance route is mounted only when the gate is configured")
        .issue();
    (axum::http::StatusCode::CREATED, Json(puzzle))
}

/// Public prefix search over the finalized username projection.
#[utoipa::path(
    get,
    path = "/api/v1/usernames/search",
    tag = "Usernames",
    params(
        SearchQuery,
        ("Authorization" = Option<String>, Header,
         description = "Optional `Bearer <JWT>` from identity-service. When proof of compute is \
            enabled, a valid token satisfies this route and no puzzle is needed. An unverifiable \
            token is treated as anonymous — this route never answers 401."),
        ("Proof-Of-Compute" = Option<String>, Header,
         description = "Solved puzzle, required only when proof of compute is enabled and no valid \
            bearer token is presented: \
            `base64(sessionId:timestamp:difficulty:counter:checksum)` for a puzzle from \
            `POST /api/v1/poc/issue`. Single-use.")
    ),
    responses(
        (status = 200, description = "Matching assigned usernames in continuation order.",
         body = SearchResponse,
         example = json!({ "usernames": [], "nextCursor": null })),
        (status = 400, description = "Invalid query parameter (problem details with `errors`), \
            invalid cursor (`{\"error\":\"Invalid cursor\"}`), or a malformed `Proof-Of-Compute` \
            header (`bad-request` problem details).",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-request-parameter-value",
             "title": "Invalid Query Parameter Value",
             "detail": "The request query contains an invalid parameter value.",
             "status": 400,
             "errors": [{ "detail": "Prefix is required", "pointer": "#/prefix" }]
         })),
        (status = 402, description = "Proof of compute is enabled and the caller presented neither a \
            valid bearer token nor an acceptable puzzle. The `detail` names the reason: missing \
            proof, checksum mismatch, expired puzzle, already-used puzzle, or insufficient \
            difficulty.",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/payment-required",
             "title": "Payment Required",
             "detail": "Proof of compute required. Request a puzzle from POST /api/v1/poc/issue and present the solved proof in the Proof-Of-Compute header.",
             "status": 402
         })),
        (status = 429, description = "Public rate limit exceeded (problem details with `Retry-After`).",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/too-many-requests",
             "title": "Too Many Requests",
             "detail": "Rate limit exceeded. Please retry after 60 seconds.",
             "status": 429
         }))
    )
)]
pub(crate) async fn search_usernames(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<SearchResponse>> {
    Ok(Json(crate::search::search(&state.pool, &params).await?))
}
