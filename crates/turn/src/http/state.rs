//! Shared application state injected into every handler.

use std::sync::Arc;

use http_common::RateLimiter;

use crate::config::{Config, ProofConfig};
use crate::credentials::Issuer;
use crate::proof::roots::RootCaches;

/// Cheap-to-clone handle to the issuer, verifier, and config. No database;
/// the only chain-facing state is the proof root cache, refreshed by a
/// background task and absent when the proof feature is off.
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
    /// Proof-authorized issuance state; `None` when `TURN_PROOF_ENABLED` is off.
    pub proof: Option<Arc<ProofState>>,
}

/// State for the proof-authorized routes.
pub struct ProofState {
    /// Derives and bounds the message a proof must be made over.
    pub freshness: crate::proof::message::Freshness,
    /// Accepted product id → the context proofs for it must be made under.
    pub contexts: std::collections::BTreeMap<String, [u8; 32]>,
    /// Latest accepted ring roots, isolated by canonical collection.
    pub roots: RootCaches,
    /// Post-verification per-alias limiter (alias = in-memory key only). The
    /// alias is context-derived, so this budget is already per person *and*
    /// product.
    pub alias_limiter: RateLimiter,
    /// Bounded ring-VRF verification slots; excess requests wait briefly.
    pub permits: Arc<tokio::sync::Semaphore>,
    /// Bounds requests waiting for a verification slot.
    pub waiters: Arc<tokio::sync::Semaphore>,
}

/// How long a caller waits for a verification slot before receiving 503.
pub const PERMIT_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
/// Maximum requests allowed to wait for a verification slot.
pub const MAX_PERMIT_WAITERS: usize = 64;

impl ProofState {
    /// Assemble proof state over an externally owned root cache (the caller
    /// decides whether a chain refresher feeds it — the bin does, tests don't).
    pub fn new(
        config: &ProofConfig,
        roots: RootCaches,
        alias_limit: u32,
        rate_window: std::time::Duration,
    ) -> Self {
        Self {
            freshness: crate::proof::message::Freshness::new(crate::config::PROOF_MAX_SKEW_SECS),
            contexts: config.contexts.clone(),
            roots,
            alias_limiter: RateLimiter::new(alias_limit, rate_window),
            permits: Arc::new(tokio::sync::Semaphore::new(config.concurrency)),
            waiters: Arc::new(tokio::sync::Semaphore::new(MAX_PERMIT_WAITERS)),
        }
    }
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
        let proof = config.proof.as_ref().map(|proof_config| {
            Arc::new(ProofState::new(
                proof_config,
                RootCaches::empty(),
                config.rate_limit,
                config.rate_window,
            ))
        });
        Self {
            issuer: Arc::new(issuer),
            verifier: Arc::new(config.jwt_verifier.clone()),
            config: Arc::new(config),
            limiter,
            proof,
        }
    }
}

impl http_common::HasJwtVerifier for AppState {
    fn jwt_verifier(&self) -> &jwt_verify::Verifier {
        &self.verifier
    }
}
