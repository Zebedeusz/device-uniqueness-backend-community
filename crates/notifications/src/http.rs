//! Axum router, state, errors, and extractors for notifications.
//!
//! Health, the request-id → trace → timeout stack, and the JWT verifier all
//! come from the shared `http-common` crate; only the notify route, its auth
//! extractor, and its error rendering are notifications-specific.

pub mod error;
pub mod state;

use axum::routing::post;
use axum::Router;
use http_common::{health, layers};

pub use self::state::AppState;

/// This service's own paths and nothing else: no health routes, no fallback, no
/// middleware stack — that is what makes it mergeable with its siblings in the
/// `all-in-one` role.
///
/// **This surface answers a different 404 from its siblings.** `routes()`
/// deliberately attaches no `fallback`, so an unmatched path under this service
/// answers axum's default *empty* 404 and a wrong method answers **405**, where
/// every other service in the workspace answers the JSON `{"error": "Not
/// found"}`. That is the live contract for `/api/v1/notify*` — the edge routes
/// the whole prefix here — so a merged router must reproduce it inside this
/// prefix rather than letting a global fallback swallow it.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/notify", post(crate::notify::handle))
        .with_state(state)
}

/// Assemble the full standalone service: [`router`] plus health at root and the
/// production middleware stack. No `fallback` — see [`router`].
pub fn routes(state: AppState) -> Router {
    let health = health::router("notifications", readiness).with_state(state.clone());
    layers::standard_layers(Router::new().merge(health).merge(router(state)))
}

/// Stateless and DB-free: if the process serves, it is ready — no external probe.
pub async fn readiness(_state: AppState) -> health::Readiness {
    Ok(&[])
}
