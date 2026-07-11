//! Shared application state.

use sqlx::PgPool;

use super::middleware::RateLimiter;
use crate::sync::Freshness;
use crate::ChainClient;

/// Cheap-to-clone database and People Chain handles for handlers.
#[derive(Clone)]
pub struct AppState {
    /// Service-owned Postgres pool.
    pub pool: PgPool,
    /// Reconnecting People Chain client.
    pub chain: ChainClient,
    /// Latest finalized-sync freshness, updated by the resync loop.
    pub freshness: Freshness,
    /// In-memory per-IP rate limiter for the public search route.
    pub limiter: RateLimiter,
}

impl AppState {
    /// Build handler state from connected dependencies and a search limiter.
    pub fn new(
        pool: PgPool,
        chain: ChainClient,
        freshness: Freshness,
        limiter: RateLimiter,
    ) -> Self {
        Self {
            pool,
            chain,
            freshness,
            limiter,
        }
    }
}
