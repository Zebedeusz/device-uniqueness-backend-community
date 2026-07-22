//! Shared application state injected into every handler.

use std::sync::Arc;

use sqlx::PgPool;

use super::middleware::RateLimiter;
use crate::chain::ChainClient;
use crate::config::Config;

/// Cheap-to-clone handle to the pool, chain client, verifier, and config.
#[derive(Clone)]
pub struct AppState {
    /// Postgres pool (this service's own `dim_tickets` database).
    pub pool: PgPool,
    /// People Chain read client (quota checks).
    pub chain: ChainClient,
    /// Verify-only JWT verifier (shared `jwt-verify` crate).
    pub verifier: Arc<jwt_verify::Verifier>,
    /// Validated runtime configuration.
    pub config: Arc<Config>,
    /// Per-subject rate limiter for the authenticated routes.
    pub limiter: RateLimiter,
}

impl AppState {
    /// Assemble state from the connected pool, chain client, and config.
    pub fn new(pool: PgPool, chain: ChainClient, config: Config) -> Self {
        let limiter = RateLimiter::new(config.rate_limit, config.rate_window);
        Self {
            pool,
            chain,
            verifier: Arc::new(config.jwt_verifier.clone()),
            config: Arc::new(config),
            limiter,
        }
    }
}
