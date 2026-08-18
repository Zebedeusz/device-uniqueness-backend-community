//! Shared axum primitives for the services' HTTP surface (invite-tickets,
//! turn, notifications).
//!
//! Extracted once a second service needed the same HTTP scaffolding — the same
//! second-consumer rule that produced `jwt-verify` and `chain-client`.
//!
//! What lives here (transport shape only — no domain logic):
//! - [`error`]: the one JSON error envelope every service renders —
//!   `{"error": …}` plus a `fields` array on per-field validation failures —
//!   and the message fragments those field errors carry.
//! - [`auth`]: the verify-only `Authorization: Bearer` JWT extractor.
//! - [`rate_limit`]: the per-subject fixed-window limiter.
//! - [`health`]: `/healthcheck`, `/livez`, `/readyz` with per-service
//!   readiness probes.
//! - [`layers`]: the standard tower-http middleware stack (request-id →
//!   trace → 30s timeout).
//! - [`metrics`]: the Prometheus recorder + `/metrics` listener on its own
//!   port, shared by the HTTP services and the headless workers alike.
//! - [`telemetry`]: the log subscriber every process installs, text locally
//!   and JSON (`LOG_FORMAT=json`) where the logs are aggregated.
//! - [`config`]: fail-fast environment parsing helpers + JWT verifier
//!   loading.

pub mod auth;
pub mod config;
pub mod error;
pub mod health;
pub mod layers;
pub mod metrics;
pub mod rate_limit;
pub mod telemetry;

pub use auth::{AuthSubject, HasJwtVerifier};
pub use error::FieldError;
pub use rate_limit::RateLimiter;
