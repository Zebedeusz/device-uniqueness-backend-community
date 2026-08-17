//! Shared axum primitives for services that speak the frozen legacy wire
//! contract (invite-tickets, turn, notifications).
//!
//! Extracted once a second service needed the same HTTP scaffolding — the same
//! second-consumer rule that produced
//! `jwt-verify` and `chain-client`. The shapes here are **frozen legacy
//! bytes**, golden-captured from the real TypeScript backend (see each
//! service's `tests/fixtures/`): change them only with a fresh capture in
//! hand.
//!
//! What lives here (transport shape only — no domain logic):
//! - [`problem`]: the legacy error dialects — RFC 9457 problem details
//!   (401 trio / 429 / 400 with field errors), the Hono plain-text bodies
//!   (`Malformed JSON in request body`, `404 Not Found`), and the opaque
//!   global-hook 500.
//! - [`zod`]: the zod v4 message fragments the 400 field errors carry.
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
pub mod health;
pub mod layers;
pub mod metrics;
pub mod problem;
pub mod rate_limit;
pub mod telemetry;
pub mod zod;

pub use auth::{AuthSubject, HasJwtVerifier};
pub use problem::FieldError;
pub use rate_limit::RateLimiter;
