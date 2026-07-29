//! HTTP surface for `identity-api`: the router assembly plus the shared axum
//! primitives (state, errors, middleware, health).

pub mod error;
pub mod health;
pub mod middleware;
pub mod state;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use http_common::layers;
use http_common::problem::hono_not_found;

use self::state::AppState;
use crate::{auth, queue, usernames};

/// Assemble the full router: health + JWKS at root, the `/api/v1` surface
/// below, wrapped in the shared production middleware stack (request id →
/// tracing → timeout). Unmatched paths fall back to the frozen Hono
/// `404 Not Found` (the legacy global 404 dialect).
pub fn routes(state: AppState) -> Router {
    let api_v1 = Router::new()
        .nest("/auth", auth::router(state.clone()))
        .route("/attester", get(usernames::attester))
        .nest("/usernames", usernames::router());
    let api_v1 = if state.config.queue_enabled {
        api_v1.route(
            "/registration/queue",
            get(queue::status).fallback(hono_not_found),
        )
    } else {
        api_v1
    };

    let router = Router::new()
        .merge(health::router())
        .route("/.well-known/jwks.json", get(jwks))
        .nest("/api/v1", api_v1)
        .fallback(hono_not_found)
        .with_state(state);
    layers::standard_layers(router)
}

/// Publish the Ed25519 public key so siblings can verify tokens.
#[utoipa::path(
    get,
    path = "/.well-known/jwks.json",
    tag = "Discovery",
    responses(
        (status = 200, description = "JWT verification keyset (Ed25519 / OKP).",
         body = serde_json::Value,
         example = json!({ "keys": [ {
             "kty": "OKP", "crv": "Ed25519", "use": "sig", "alg": "EdDSA",
             "kid": "...", "x": "..."
         } ] }))
    )
)]
pub(crate) async fn jwks(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.jwt.jwks())
}
