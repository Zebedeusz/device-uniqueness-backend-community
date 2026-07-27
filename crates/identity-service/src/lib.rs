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
pub mod http;
pub mod jwt;
pub mod openapi;
pub mod usernames;

pub use chain::ChainClient;
pub use config::Config;
pub use http::routes;
pub use http::state::AppState;
pub use jwt::Jwt;
