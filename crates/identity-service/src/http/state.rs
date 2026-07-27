//! Shared application state injected into every handler.

use std::sync::Arc;

use sqlx::PgPool;

use super::middleware::RateLimiter;
use crate::chain::ChainClient;
use crate::config::Config;
use crate::jwt::Jwt;

/// Cheap-to-clone handle to the pool, signer, config, and rate limiter.
#[derive(Clone)]
pub struct AppState {
    /// Postgres pool (challenges, refresh tokens).
    pub pool: PgPool,
    /// People Chain read client.
    pub chain: ChainClient,
    /// JWT issuer (Ed25519).
    pub jwt: Arc<Jwt>,
    /// Validated runtime configuration.
    pub config: Arc<Config>,
    /// Per-route rate limiter for the auth handshake.
    pub limiter: RateLimiter,
}

/// Lets the usernames surface use `http-common`'s `AuthSubject` extractor
/// (the frozen 401 trio) against this issuer's own verifying key.
impl http_common::HasJwtVerifier for AppState {
    fn jwt_verifier(&self) -> &jwt_verify::Verifier {
        self.jwt.verifier()
    }
}

impl AppState {
    /// Assemble state from the connected pool, chain client, signer, and config.
    pub fn new(pool: PgPool, chain: ChainClient, jwt: Jwt, config: Config) -> Self {
        let limiter = RateLimiter::new(config.auth_rate_limit, config.auth_rate_window);
        Self {
            pool,
            chain,
            jwt: Arc::new(jwt),
            config: Arc::new(config),
            limiter,
        }
    }
}
