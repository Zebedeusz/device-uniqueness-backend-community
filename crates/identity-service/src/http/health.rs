//! Health, liveness, and readiness endpoints.
//!
//! `livez` answers "should this pod be restarted", `readyz` answers "can this
//! instance take traffic". `readyz` gates on a live Postgres check; People Chain
//! connectivity is added to it in the identity-chain-writer slice.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;

use super::state::AppState;

/// Build the health router (mounted at the root, not under `/api/v1`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthcheck", get(healthcheck))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
    service: &'static str,
}

async fn healthcheck() -> Json<Status> {
    Json(Status {
        status: "ok",
        service: "identity-service",
    })
}

async fn livez() -> Json<Status> {
    Json(Status {
        status: "alive",
        service: "identity-service",
    })
}

async fn readyz(State(state): State<AppState>) -> Response {
    if let Err(err) = sqlx::query("SELECT 1").execute(&state.pool).await {
        tracing::warn!(error = ?err, "readiness check failed: database unavailable");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "service": "identity-service", "db": "down" })),
        )
            .into_response();
    }
    if let Err(err) = state.chain.health().await {
        tracing::warn!(error = ?err, "readiness check failed: People Chain unavailable");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "unavailable", "service": "identity-service", "chain": "down" }),
            ),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        Json(
            json!({ "status": "ready", "service": "identity-service", "db": "up", "chain": "up" }),
        ),
    )
        .into_response()
}
