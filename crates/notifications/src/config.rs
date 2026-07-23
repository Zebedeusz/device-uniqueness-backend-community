//! Runtime configuration for the notifications relay.
//!
//! Fail-fast, like the sibling services (via the shared `http-common` helpers):
//! a missing or malformed value stops the process loudly at boot. The relay is
//! stateless and DB-free, so the only required material is the identity-service
//! JWT **public** key (or JWKS) it verifies bearer tokens against.

use std::net::SocketAddr;
use std::time::Duration;

pub use http_common::config::ConfigError;
use http_common::config::{jwt_verifier_from_env, parse_var, positive};

/// Validated notifications runtime configuration.
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Verify-only JWT verifier (`JWT_JWKS_JSON` wins, else `JWT_ED25519_PUBLIC_KEY`).
    pub jwt_verifier: jwt_verify::Verifier,
    /// Requests per window per authenticated subject on `/api/v1/notify`.
    pub rate_limit: u32,
    /// Rate-limit window for `/api/v1/notify`.
    pub rate_window: Duration,
}

impl Config {
    /// Read and validate configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let rate_limit = positive("NOTIFY_RATE_LIMIT", parse_var("NOTIFY_RATE_LIMIT", "60")?)?;
        let rate_window_secs: u64 = positive(
            "NOTIFY_RATE_LIMIT_WINDOW_SECS",
            parse_var("NOTIFY_RATE_LIMIT_WINDOW_SECS", "60")?,
        )?;
        Ok(Self {
            bind_addr: parse_var("BIND_ADDR", "0.0.0.0:8080")?,
            jwt_verifier: jwt_verifier_from_env()?,
            rate_limit,
            rate_window: Duration::from_secs(rate_window_secs),
        })
    }
}

/// Read a required value through a caller-provided lookup, rejecting empty ones.
///
/// The APNs/FCM provider configs take a getter closure so their env parsing is
/// unit-testable; this is the getter-based counterpart to `http-common`'s
/// env-based `required_var`.
pub(crate) fn required<F>(get: &F, key: &'static str) -> Result<String, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::Missing(key))
}
