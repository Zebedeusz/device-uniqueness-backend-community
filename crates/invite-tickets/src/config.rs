//! Runtime configuration for invite-tickets, loaded and validated from the
//! environment at startup.
//!
//! Loading is fail-fast (same posture as identity-service): a missing or
//! malformed required value stops the process loudly at boot instead of
//! serving with half-initialised state.
//!
//! The **api** bin needs no chain access and no inviter identity — tickets
//! carry their inviter in-row and claims never touch the People Chain. Chain
//! RPC and the inviter secret belong to the pool-maintainer bin's own config
//! (Phase 2), keeping the signing secret out of the N-replica API entirely.

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use crate::tickets::Network;

/// invite-tickets api runtime configuration.
#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Postgres connection string (required; no default). This service's own DB.
    pub database_url: String,
    /// Network literal selecting the `(dim, network)` pools this deployment
    /// serves and stamped into responses.
    pub network: Network,
    /// Verify-only JWT key material (required; no default — fail closed).
    pub jwt_verifier: jwt_verify::Verifier,
    /// Max requests per authenticated subject per window.
    pub rate_limit: u32,
    /// Rate-limit window.
    pub rate_window: Duration,
}

/// The DB password inside `database_url` must never reach logs, spans, or
/// error output — a `{:?}` of the config anywhere would otherwise leak it, so
/// `Debug` is implemented by hand. (The JWT material here is the public
/// verification key; the api bin holds no signing secret at all.)
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("network", &self.network)
            .field("jwt_verifier", &self.jwt_verifier)
            .field("rate_limit", &self.rate_limit)
            .field("rate_window", &self.rate_window)
            .finish()
    }
}

/// A required or malformed configuration value; surfaced at startup.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A required environment variable was not set.
    #[error("required environment variable {0} is not set")]
    Missing(&'static str),
    /// An environment variable held an unparseable value.
    #[error("environment variable {key} is invalid: {reason}")]
    Invalid {
        /// The offending variable name.
        key: &'static str,
        /// Why it could not be parsed.
        reason: String,
    },
}

impl Config {
    /// Read and validate configuration from the environment.
    ///
    /// Fails (rather than defaulting) for `DATABASE_URL`, `PEOPLE_NETWORK`,
    /// and the JWT key material (`JWT_JWKS_JSON` or `JWT_ED25519_PUBLIC_KEY`).
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = required_var("DATABASE_URL")?;

        let network_raw = required_var("PEOPLE_NETWORK")?;
        let network = Network::from_str(&network_raw).map_err(|_| ConfigError::Invalid {
            key: "PEOPLE_NETWORK",
            reason: format!("expected westend2|paseo|polkadot, got {network_raw}"),
        })?;

        let rate_limit: u32 = positive("RATE_LIMIT", parse_var("RATE_LIMIT", "30")?)?;
        let rate_window_secs: u64 = positive(
            "RATE_LIMIT_WINDOW_SECS",
            parse_var("RATE_LIMIT_WINDOW_SECS", "60")?,
        )?;
        Ok(Self {
            bind_addr: parse_var("BIND_ADDR", "0.0.0.0:8080")?,
            database_url,
            network,
            jwt_verifier: jwt_verifier_from_env()?,
            rate_limit,
            rate_window: Duration::from_secs(rate_window_secs),
        })
    }
}

/// Load verify-only key material: `JWT_JWKS_JSON` (an inline JWKS document, as
/// served by identity-service at `/.well-known/jwks.json`) wins; otherwise
/// `JWT_ED25519_PUBLIC_KEY` (32-byte hex, optional `0x`) pins a single key.
fn jwt_verifier_from_env() -> Result<jwt_verify::Verifier, ConfigError> {
    let jwks = std::env::var("JWT_JWKS_JSON").ok();
    let public_key = std::env::var("JWT_ED25519_PUBLIC_KEY").ok();
    jwt_verifier_from_values(jwks.as_deref(), public_key.as_deref())
}

fn jwt_verifier_from_values(
    jwks: Option<&str>,
    public_key: Option<&str>,
) -> Result<jwt_verify::Verifier, ConfigError> {
    if let Some(jwks) = jwks.filter(|value| !value.trim().is_empty()) {
        return jwt_verify::Verifier::from_jwks(jwks).map_err(|e| ConfigError::Invalid {
            key: "JWT_JWKS_JSON",
            reason: e.to_string(),
        });
    }

    let raw = public_key.ok_or(ConfigError::Missing(
        "JWT_JWKS_JSON or JWT_ED25519_PUBLIC_KEY",
    ))?;
    let trimmed = raw.trim();
    let bytes: [u8; 32] = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| ConfigError::Invalid {
            key: "JWT_ED25519_PUBLIC_KEY",
            reason: "expected 32 bytes of hex".to_string(),
        })?;
    let key =
        ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(|e| ConfigError::Invalid {
            key: "JWT_ED25519_PUBLIC_KEY",
            reason: format!("not a valid Ed25519 public key: {e}"),
        })?;
    Ok(jwt_verify::Verifier::from_verifying_key(key))
}

fn required_var(key: &'static str) -> Result<String, ConfigError> {
    let raw = std::env::var(key).map_err(|_| ConfigError::Missing(key))?;
    let value = raw.trim();
    if value.is_empty() {
        return Err(ConfigError::Invalid {
            key,
            reason: "must not be empty".to_string(),
        });
    }
    Ok(value.to_string())
}

/// Parse an env var into `T`, falling back to `default` when unset.
fn parse_var<T>(key: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = std::env::var(key).unwrap_or_else(|_| default.to_string());
    raw.parse().map_err(|e: T::Err| ConfigError::Invalid {
        key,
        reason: e.to_string(),
    })
}

fn positive<T>(key: &'static str, value: T) -> Result<T, ConfigError>
where
    T: Default + PartialEq,
{
    if value == T::default() {
        return Err(ConfigError::Invalid {
            key,
            reason: "must be at least 1".to_string(),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_operational_values() {
        for key in ["RATE_LIMIT", "RATE_LIMIT_WINDOW_SECS"] {
            assert!(matches!(
                positive(key, 0_u64),
                Err(ConfigError::Invalid { key: found, reason })
                    if found == key && reason == "must be at least 1"
            ));
        }
    }

    #[test]
    fn empty_jwks_falls_back_to_pinned_public_key() {
        let public_key = "0x8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";
        assert!(jwt_verifier_from_values(Some("  "), Some(public_key)).is_ok());
    }

    #[test]
    fn missing_key_material_is_a_hard_error() {
        assert!(matches!(
            jwt_verifier_from_values(None, None),
            Err(ConfigError::Missing(_))
        ));
    }
}
