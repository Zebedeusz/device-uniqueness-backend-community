//! invite-tickets — the synchronous invitation-credential claim service.
//!
//! Preserves the legacy `POST /api/v1/invitation-ticket/claim` mobile contract
//! (the route the shipping iOS/Android apps call for `Game` / `ProofOfInk`
//! credentials) as an independently deployable service: its own Postgres
//! keypair pool (`available → claimed`, two states only), verify-only JWT auth
//! via the shared `jwt-verify` crate, and a single-instance pool maintainer
//! (Phase 2) that generates keypairs and registers them on People Chain
//! before they become claimable.
//!
//! Plan: `docs/plans/active/invite-claim.md`. Boundaries and invariants:
//! `docs/architecture.md`.

pub mod chain;
pub mod config;
pub mod db;
pub mod http;
pub mod openapi;
pub mod pool;
pub mod sign;
pub mod tickets;

pub use config::Config;
pub use http::routes;
pub use http::state::AppState;
