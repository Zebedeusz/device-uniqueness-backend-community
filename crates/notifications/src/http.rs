//! Axum router, state, errors, and extractors for notifications.
//!
//! Health, the request-id → trace → timeout stack, and the JWT verifier all
//! come from the shared `http-common` crate; only the notify route, its auth
//! extractor, and its frozen error dialect are notifications-specific.

pub mod error;
pub mod state;

use axum::routing::post;
use axum::Router;
use http_common::{health, layers};

pub use self::state::AppState;

/// Assemble the health endpoints and the authenticated notify route, wrapped in
/// the standard production middleware stack.
pub fn routes(state: AppState) -> Router {
    let router = Router::new()
        .merge(health::router("notifications", readiness))
        .route("/api/v1/notify", post(crate::notify::handle))
        .with_state(state);
    layers::standard_layers(router)
}

/// Stateless and DB-free: if the process serves, it is ready — no external probe.
async fn readiness(_state: AppState) -> health::Readiness {
    Ok(&[])
}
