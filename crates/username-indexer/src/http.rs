//! Axum router, state, errors, and health endpoints for username-indexer.

pub mod error;
pub mod health;
pub mod middleware;
pub mod state;

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use http_common::layers;
use http_common::problem::hono_not_found;

use self::error::AppResult;
pub use self::state::AppState;
use crate::search::{SearchQuery, SearchResponse};

/// Assemble health endpoints and the public username read API.
///
/// The in-memory per-IP rate limiter wraps the public search route, so the
/// health endpoints stay unthrottled. Unmatched paths and methods fall back
/// to the frozen Hono `404 Not Found` — which is also what serves the two
/// deliberately-dropped legacy endpoints: the list (`GET /api/v1/usernames`)
/// and the single-username lookup (`GET /api/v1/usernames/{username}`,
/// deprecated in favour of paginated `search`).
pub fn routes(state: AppState) -> Router {
    // Method fallbacks keep wrong-method requests on the Hono 404 dialect
    // (Hono routes were method-specific: never a 405).
    let api = Router::new()
        .route(
            "/api/v1/usernames/search",
            get(search_usernames).fallback(hono_not_found),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit,
        ));
    let router = Router::new()
        .merge(health::router())
        .merge(api)
        .fallback(hono_not_found)
        .with_state(state);
    layers::standard_layers(router)
}

/// Public prefix search over the finalized username projection.
#[utoipa::path(
    get,
    path = "/api/v1/usernames/search",
    tag = "Usernames",
    params(SearchQuery),
    responses(
        (status = 200, description = "Matching assigned usernames in continuation order.",
         body = SearchResponse,
         example = json!({ "usernames": [], "nextCursor": null })),
        (status = 400, description = "Invalid query parameter (problem details with `errors`) or invalid cursor (`{\"error\":\"Invalid cursor\"}`).",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-request-parameter-value",
             "title": "Invalid Query Parameter Value",
             "detail": "The request query contains an invalid parameter value.",
             "status": 400,
             "errors": [{ "detail": "Prefix is required", "pointer": "#/prefix" }]
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
