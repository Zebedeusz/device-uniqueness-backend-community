//! HTTP surface for `identity-api`: the router assembly plus the shared axum
//! primitives (state, errors, extractors, middleware, health).

pub mod error;
pub mod extract;
pub mod health;
pub mod middleware;
pub mod state;

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use self::state::AppState;
use crate::{auth, usernames};

/// Per-request timeout; a slow handler yields `408` instead of pinning a connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble the full router: health + JWKS at root, the `/api/v1` surface below,
/// wrapped in the production middleware stack (request id → tracing → timeout).
pub fn routes(state: AppState) -> Router {
    let api_v1 = Router::new()
        .nest("/auth", auth::router(state.clone()))
        .route("/attester", get(usernames::attester))
        .nest("/usernames", usernames::router(state.clone()));

    Router::new()
        .merge(health::router())
        .route("/.well-known/jwks.json", get(jwks))
        .nest("/api/v1", api_v1)
        // Layers run outermost-first, and the last `.layer` added is the outermost.
        // So a fresh `x-request-id` is minted first, the tracing span picks it up,
        // and the timeout wraps only the handler work.
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

/// Per-request tracing span carrying the request id, method, and path. All logs
/// emitted while handling the request inherit these fields.
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

/// Publish the Ed25519 public key so siblings can verify tokens.
async fn jwks(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.jwt.jwks())
}
