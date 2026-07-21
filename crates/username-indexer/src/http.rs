//! Axum router, state, errors, and health endpoints for username-indexer.

pub mod error;
pub mod health;
pub mod middleware;
pub mod state;

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use self::error::AppResult;
pub use self::state::AppState;
use crate::search::{SearchQuery, SearchResponse};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble health endpoints and the public username search API.
///
/// The in-memory rate limiter is applied via `route_layer` to the search route
/// only, so the health endpoints (`/livez`, `/readyz`, `/healthcheck`) stay
/// unthrottled. The outer stack (request-id → trace → timeout) wraps everything.
pub fn routes(state: AppState) -> Router {
    let search = Router::new()
        .route("/api/v1/usernames/search", get(search_usernames))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit,
        ));
    Router::new()
        .merge(health::router())
        .merge(search)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_http_span)
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
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
        (status = 400, description = "Missing/invalid prefix, status, limit, or cursor.",
         body = serde_json::Value,
         example = json!({ "error": "WRONG_DATA", "message": "prefix must contain between 1 and 64 characters" })),
        (status = 429, description = "Public search rate limit exceeded.",
         body = serde_json::Value,
         example = json!({ "error": "RATE_LIMITED", "message": "rate limited" }))
    )
)]
pub(crate) async fn search_usernames(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> AppResult<Json<SearchResponse>> {
    Ok(Json(crate::search::search(&state.pool, query).await?))
}

fn make_http_span(request: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %request.method(),
        path = request.uri().path(),
    )
}
