//! Shared application state injected into every handler.

use std::sync::Arc;

use http_common::RateLimiter;

use crate::config::Config;
use crate::credentials::Issuer;

/// Cheap-to-clone handle to the issuer, verifier, and config. No database,
/// no chain client — this service is fully stateless.
#[derive(Clone)]
pub struct AppState {
    /// The credential minter (holds the relay-shared HMAC secret).
    pub issuer: Arc<Issuer>,
    /// Verify-only JWT verifier (shared `jwt-verify` crate).
    pub verifier: Arc<jwt_verify::Verifier>,
    /// Validated runtime configuration.
    pub config: Arc<Config>,
    /// Per-subject rate limiter for the authenticated route.
    pub limiter: RateLimiter,
}

impl AppState {
    /// Assemble state from the validated config.
    pub fn new(config: Config) -> Self {
        let limiter = RateLimiter::new(config.rate_limit, config.rate_window);
        let issuer = Issuer::new(
            config.turn_secret.clone(),
            config.algorithm,
            config.ttl_secs,
        );
        Self {
            issuer: Arc::new(issuer),
            verifier: Arc::new(config.jwt_verifier.clone()),
            config: Arc::new(config),
            limiter,
        }
    }
}

impl http_common::HasJwtVerifier for AppState {
    fn jwt_verifier(&self) -> &jwt_verify::Verifier {
        &self.verifier
    }
}
