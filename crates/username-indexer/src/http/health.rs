//! Liveness and dependency readiness endpoints.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;

use super::AppState;

/// Build root health, liveness, and readiness routes.
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
        service: "username-indexer",
    })
}

async fn livez() -> Json<Status> {
    Json(Status {
        status: "alive",
        service: "username-indexer",
    })
}

/// The dependency probe behind `/readyz` and the readiness gauges: Postgres,
/// then the People Chain RPC.
pub async fn probe(
    pool: sqlx::PgPool,
    chain: crate::ChainClient,
) -> http_common::health::Readiness {
    if let Err(error) = sqlx::query("SELECT 1").execute(&pool).await {
        tracing::warn!(error = ?error, dependency = "postgres", "readiness check failed");
        return Err("db");
    }
    if let Err(error) = chain.health().await {
        tracing::warn!(error = ?error, dependency = "people_chain", "readiness check failed");
        return Err("chain");
    }
    Ok(&["db", "chain"])
}

async fn readyz(State(state): State<AppState>) -> Response {
    match probe(state.pool.clone(), state.chain.clone()).await {
        Err("db") => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unavailable",
                    "service": "username-indexer",
                    "db": "down"
                })),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unavailable",
                    "service": "username-indexer",
                    "chain": "down"
                })),
            )
                .into_response()
        }
        Ok(_) => {}
    }
    let freshness =
        serde_json::to_value(state.freshness.snapshot()).unwrap_or(serde_json::Value::Null);
    (
        StatusCode::OK,
        Json(json!({
            "status": "ready",
            "service": "username-indexer",
            "db": "up",
            "chain": "up",
            "freshness": freshness
        })),
    )
        .into_response()
}
