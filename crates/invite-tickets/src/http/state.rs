//! Shared application state injected into every handler.

use std::sync::Arc;

use http_common::RateLimiter;
use sqlx::PgPool;

use crate::config::Config;

/// Cheap-to-clone handle to the pool, verifier, and config.
///
/// Note there is **no chain client here**: the claim path is DB + signing
/// only. All chain interaction lives in the single-instance pool-maintainer
/// binary.
#[derive(Clone)]
pub struct AppState {
    /// Postgres pool (this service's own `invite_tickets` database).
    pub pool: PgPool,
    /// Verify-only JWT verifier (shared `jwt-verify` crate).
    pub verifier: Arc<jwt_verify::Verifier>,
    /// Validated runtime configuration.
    pub config: Arc<Config>,
    /// Per-subject rate limiter for the authenticated route.
    pub limiter: RateLimiter,
}

impl AppState {
    /// Assemble state from the connected pool and config.
    pub fn new(pool: PgPool, config: Config) -> Self {
        let limiter = RateLimiter::new(config.rate_limit, config.rate_window);
        Self {
            pool,
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
