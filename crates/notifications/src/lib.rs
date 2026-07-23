//! Thin, stateless, verify-only `/api/v1/notify` relay for Identity Backend V2.
//!
//! Validates identity-service's Ed25519 JWT with public-key material only (never
//! mints tokens, calls identity-service, or touches a database), preserves the
//! frozen mobile request/response contract, and forwards the flat payload to a
//! [`PushProvider`].

pub mod config;
pub mod http;
pub mod notify;
pub mod openapi;
pub mod platform;
pub mod provider;

pub use config::{Config, ConfigError};
pub use http::{routes, AppState};
pub use http_common::RateLimiter;
pub use jwt_verify::{JwtError, VerifiedClaims, Verifier};
pub use notify::{NotifyResponse, PushError};
pub use platform::Platform;
pub use provider::apns::{ApnsConfig, ApnsEnvironment, ApnsProvider};
pub use provider::fcm::{FcmConfig, FcmProvider};
pub use provider::{
    ProviderError, PushOutcome, PushProvider, PushRequest, RecordingProvider, UnconfiguredProvider,
};
