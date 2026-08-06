//! identity-service — Identity Backend V2 (Rust).
//!
//! Shared domain and HTTP surface for the identity service: device attestation ->
//! JWT -> username registration on People Chain. It is consumed by two
//! binaries — `identity-api` (the N-replica axum server) and `identity-chain-writer`
//! (the single-instance worker that drains the reservation outbox onto chain).
//! Attestation is gated by [`config::Config::auth_enabled`] / `enforce_auth`.

mod auth;
pub mod chain;
pub mod config;
pub mod db;
pub mod device_check;
pub mod eligibility;
pub mod http;
pub mod jwt;
pub mod openapi;
pub mod payment;
pub mod queue;
pub mod usernames;

pub use chain::ChainClient;
pub use config::Config;
pub use http::routes;
pub use http::state::AppState;
pub use jwt::Jwt;

/// Test-only surface for the live-Postgres suite (`tests/auth_live.rs`); not
/// part of the crate's API.
#[doc(hidden)]
pub use auth::app_attest::store as app_attest_store;

/// Serializes tests that mutate process-global environment variables
/// (`Config::from_env`, `WriterConfig::from_env`): cargo runs `#[test]`s on
/// parallel threads, so unlocked env mutation races across modules.
#[cfg(test)]
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
