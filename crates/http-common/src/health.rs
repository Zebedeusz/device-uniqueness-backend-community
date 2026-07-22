//! Health, liveness, and readiness endpoints.
//!
//! `livez` answers "should this pod be restarted", `readyz` answers "can this
//! instance take traffic". What readiness gates on is each service's call —
//! the probe closure runs the checks (and logs its own failures) and reports
//! either every healthy component or the first broken one.

use std::future::Future;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

/// The outcome of a readiness probe: every component that answered (in report
/// order), or the first one that did not.
pub type Readiness = Result<&'static [&'static str], &'static str>;

/// Build the health router (mounted at the root, not under `/api/v1`).
///
/// `ready` is the service's readiness probe: it runs the actual checks
/// (Postgres, chain RPC, …), logs failures itself, and returns
/// `Ok(&["db", …])` or `Err("db")` — rendered as the fixed
/// `{"status", "service", <component>: "up"|"down"}` bodies.
pub fn router<S, F, Fut>(service: &'static str, ready: F) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(S) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Readiness> + Send + 'static,
{
    Router::new()
        .route(
            "/healthcheck",
            get(move || async move { status_body(service, "ok") }),
        )
        .route(
            "/livez",
            get(move || async move { status_body(service, "alive") }),
        )
        .route(
            "/readyz",
            get(move |State(state): State<S>| {
                let ready = ready.clone();
                async move {
                    match ready(state).await {
                        Ok(components) => {
                            let mut body = json!({ "status": "ready", "service": service });
                            for component in components {
                                body[component] = json!("up");
                            }
                            (StatusCode::OK, Json(body)).into_response()
                        }
                        Err(component) => {
                            let mut body = json!({ "status": "unavailable", "service": service });
                            body[component] = json!("down");
                            (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
                        }
                    }
                }
            }),
        )
}

fn status_body(service: &'static str, status: &'static str) -> Response {
    Json(json!({ "status": status, "service": service })).into_response()
}
