//! The standard production middleware stack: request-id → trace → timeout.

use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Per-request timeout; a slow handler yields `408` instead of pinning a connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Wrap a fully-assembled router in the standard stack (outermost first:
/// set request id → propagate it → trace → 30s timeout).
pub fn standard_layers(router: Router) -> Router {
    router
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
}

/// Per-request tracing span carrying the request id, method, and path.
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
