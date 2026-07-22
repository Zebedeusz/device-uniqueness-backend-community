//! dim-tickets — the game/personhood invitation-credential service.
//!
//! Preserves the legacy `/api/v1/dim-ticket` wire contract (request + status
//! of `Game` / `ProofOfInk` invitation credentials) as an independently
//! deployable service: its own Postgres state machine
//! (`PENDING → SUBMITTING → REGISTERED | retry | FAILED`), verify-only JWT auth
//! via the shared `jwt-verify` crate, and a single-instance chain writer
//! (Phase 2) that registers pending tickets on People Chain.
//!
//! Plan: `docs/plans/active/dim-tickets.md`. Boundaries and invariants:
//! `docs/architecture.md`.

pub mod chain;
pub mod config;
pub mod db;
pub mod http;
pub mod plan;
pub mod tickets;
pub mod writer;

pub use chain::ChainClient;
pub use config::Config;
pub use http::routes;
pub use http::state::AppState;
