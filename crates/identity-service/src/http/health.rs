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
use utoipa::ToSchema;

use super::state::AppState;

/// Build the health router (mounted at the root, not under `/api/v1`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthcheck", get(healthcheck))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
}

/// A liveness/health status body: `{ status, service }`.
#[derive(Serialize, ToSchema)]
pub(crate) struct Status {
    /// Coarse status word, e.g. `ok`, `alive`, `ready`.
    status: &'static str,
    /// Service name emitting the status.
    service: &'static str,
}

/// Process health probe. No dependencies checked.
#[utoipa::path(
    get,
    path = "/healthcheck",
    tag = "Liveness & Readiness",
    responses(
        (status = 200, description = "Process is up. No dependencies checked.", body = Status,
         example = json!({ "status": "ok", "service": "identity-service" }))
    )
)]
pub(crate) async fn healthcheck() -> Json<Status> {
    Json(Status {
        status: "ok",
        service: "identity-service",
    })
}

/// Kubernetes liveness probe — "should this pod be restarted".
#[utoipa::path(
    get,
    path = "/livez",
    tag = "Liveness & Readiness",
    responses(
        (status = 200, description = "Process is alive.", body = Status,
         example = json!({ "status": "alive", "service": "identity-service" }))
    )
)]
pub(crate) async fn livez() -> Json<Status> {
    Json(Status {
        status: "alive",
        service: "identity-service",
    })
}

/// Kubernetes readiness probe — checks `db` and `chain` subsystems.
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "Liveness & Readiness",
    responses(
        (status = 200, description = "Instance can take traffic; db and chain reachable.",
         body = serde_json::Value,
         example = json!({ "status": "ready", "service": "identity-service", "db": "up", "chain": "up" })),
        (status = 503, description = "A required subsystem is unavailable.",
         body = serde_json::Value,
         example = json!({ "status": "unavailable", "service": "identity-service", "db": "down" }))
    )
)]
pub(crate) async fn readyz(State(state): State<AppState>) -> Response {
    match probe(state.pool.clone(), state.chain.clone()).await {
        Err("db") => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "service": "identity-service", "db": "down" })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "unavailable", "service": "identity-service", "chain": "down" }),
            ),
        )
            .into_response(),
        Ok(_) => (
            StatusCode::OK,
            Json(
                json!({ "status": "ready", "service": "identity-service", "db": "up", "chain": "up" }),
            ),
        )
            .into_response(),
    }
}

/// The dependency probe behind `/readyz` and the readiness gauges: Postgres,
/// then the People Chain RPC. Shared with the workers' gauge probe so the two
/// can never disagree.
pub async fn probe(
    pool: sqlx::PgPool,
    chain: crate::ChainClient,
) -> http_common::health::Readiness {
    if let Err(err) = sqlx::query("SELECT 1").execute(&pool).await {
        tracing::warn!(error = ?err, "readiness check failed: database unavailable");
        return Err("db");
    }
    if let Err(err) = chain.health().await {
        tracing::warn!(error = ?err, "readiness check failed: People Chain unavailable");
        return Err("chain");
    }
    Ok(&["db", "chain"])
}
