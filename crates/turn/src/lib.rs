//! turn — the stateless TURN credential issuer.
//!
//! Serves `POST /api/v1/turn/issue` (short-lived coturn REST-API credentials
//! for WebRTC ICE negotiation) as an independently deployable service:
//! verify-only JWT auth via the shared `jwt-verify` crate,
//! HMAC credential minting over a secret shared with the TURN relay, and no
//! persistence at all — N replicas need no coordination.
//!
//! Plan: `docs/plans/done/turn.md`. Boundaries and invariants:
//! `docs/architecture.md`.

pub mod config;
pub mod credentials;
pub mod http;
pub mod openapi;
pub mod proof;

pub use config::Config;
pub use http::routes;
pub use http::state::AppState;
