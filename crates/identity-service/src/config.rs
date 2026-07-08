//! Runtime configuration for identity-service, loaded and validated from the
//! environment at startup.
//!
//! Loading is fail-fast: [`Config::from_env`] returns an error (rather than
//! serving with half-initialised state) when a required secret is missing or
//! malformed. This is the 2026-01-28 outage lesson — a bad/absent env value
//! must stop the process loudly, not crash-loop or half-start.

use std::net::SocketAddr;
use std::time::Duration;

/// identity-service runtime configuration.
///
/// `auth_enabled` and `enforce_auth` mirror the current backend's attestation
/// gates so the same binary runs attestation-off on testnets and hard-gated in
/// prod:
/// - `auth_enabled = false` makes all platform attestation a no-op (a JWT is
///   still issued; the sr25519 account proof is still verified).
/// - `enforce_auth = false` is the soft/advisory mode (log the attestation
///   verdict, do not reject); `true` hard-rejects bad or missing attestation.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Postgres connection string (required; no default).
    pub database_url: String,
    /// Ed25519 seed used to sign JWTs (32 bytes). identity-service is the sole issuer.
    pub jwt_secret: [u8; 32],
    /// JWT `iss` claim; kept as the current backend's value for compatibility.
    pub jwt_issuer: String,
    /// People Chain RPC endpoint for read-path chain queries.
    pub people_rpc_url: String,
    /// Attester authority public key, normalised as `0x`+hex (32 bytes).
    pub attester_public_key: String,
    /// Access-token lifetime.
    pub access_ttl: Duration,
    /// Refresh-token lifetime.
    pub refresh_ttl: Duration,
    /// Attestation challenge lifetime.
    pub challenge_ttl: Duration,
    /// Max requests per client per window on the unauthenticated auth routes.
    pub auth_rate_limit: u32,
    /// Rate-limit window for the auth routes.
    pub auth_rate_window: Duration,
    /// Master attestation switch; `false` = attestation is a no-op.
    pub auth_enabled: bool,
    /// Hard gate (`true`) vs soft/advisory gate (`false`).
    pub enforce_auth: bool,
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
    /// Fails (rather than defaulting) for `DATABASE_URL` and
    /// `JWT_ED25519_SECRET`; everything else has a safe local default.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr = parse_var("BIND_ADDR", "0.0.0.0:8080")?;
        let database_url =
            std::env::var("DATABASE_URL").map_err(|_| ConfigError::Missing("DATABASE_URL"))?;
        let jwt_secret = parse_jwt_secret()?;

        Ok(Self {
            bind_addr,
            database_url,
            jwt_secret,
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "polkadot-app".to_string()),
            people_rpc_url: std::env::var("PEOPLE_RPC_URL")
                .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string()),
            attester_public_key: parse_attester_public_key()?,
            access_ttl: Duration::from_secs(parse_var("ACCESS_TOKEN_TTL_SECS", "86400")?),
            refresh_ttl: Duration::from_secs(parse_var("REFRESH_TOKEN_TTL_SECS", "2592000")?),
            challenge_ttl: Duration::from_secs(parse_var("CHALLENGE_TTL_SECS", "300")?),
            auth_rate_limit: parse_var("AUTH_RATE_LIMIT", "30")?,
            auth_rate_window: Duration::from_secs(parse_var("AUTH_RATE_WINDOW_SECS", "60")?),
            auth_enabled: env_bool("AUTH_ENABLED", false),
            enforce_auth: env_bool("ENFORCE_AUTH", false),
        })
    }

    /// Human-readable attestation posture for startup logs and `/readyz` detail.
    pub fn attestation_mode(&self) -> &'static str {
        match (self.auth_enabled, self.enforce_auth) {
            (false, _) => "disabled (attestation is a no-op; JWT still issued)",
            (true, false) => "soft (verdicts logged, not enforced)",
            (true, true) => "hard (bad or missing attestation rejected)",
        }
    }
}

/// Parse `JWT_ED25519_SECRET` (hex or base64) into a 32-byte Ed25519 seed.
fn parse_jwt_secret() -> Result<[u8; 32], ConfigError> {
    use base64::Engine as _;

    let raw = std::env::var("JWT_ED25519_SECRET")
        .map_err(|_| ConfigError::Missing("JWT_ED25519_SECRET"))?;
    let trimmed = raw.trim();
    let bytes = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed))
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD
                .decode(trimmed)
                .ok()
        })
        .ok_or_else(|| ConfigError::Invalid {
            key: "JWT_ED25519_SECRET",
            reason: "expected 32 bytes as hex or base64".to_string(),
        })?;

    bytes.try_into().map_err(|_| ConfigError::Invalid {
        key: "JWT_ED25519_SECRET",
        reason: "decoded secret must be exactly 32 bytes".to_string(),
    })
}

/// Parse `ATTESTER_PUBLIC_KEY` (hex, optional `0x`) into a normalised `0x`+hex
/// 32-byte string served by `GET /api/v1/attester`.
fn parse_attester_public_key() -> Result<String, ConfigError> {
    let raw = std::env::var("ATTESTER_PUBLIC_KEY")
        .map_err(|_| ConfigError::Missing("ATTESTER_PUBLIC_KEY"))?;
    let trimmed = raw.trim();
    let bytes = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed)).map_err(|e| {
        ConfigError::Invalid {
            key: "ATTESTER_PUBLIC_KEY",
            reason: format!("expected hex: {e}"),
        }
    })?;
    if bytes.len() != 32 {
        return Err(ConfigError::Invalid {
            key: "ATTESTER_PUBLIC_KEY",
            reason: format!("expected 32 bytes, got {}", bytes.len()),
        });
    }
    Ok(format!("0x{}", hex::encode(bytes)))
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

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => default,
    }
}
