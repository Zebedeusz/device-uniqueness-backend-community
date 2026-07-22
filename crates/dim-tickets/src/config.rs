//! Runtime configuration for dim-tickets, loaded and validated from the
//! environment at startup.
//!
//! Loading is fail-fast (via the shared `http-common` config helpers): a
//! missing or malformed required value stops the process loudly at boot
//! instead of serving with half-initialised state.

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

pub use http_common::config::ConfigError;
use http_common::config::{jwt_verifier_from_env, parse_var, positive, required_var};

use crate::tickets::Network;

/// dim-tickets api runtime configuration.
#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Postgres connection string (required; no default). This service's own DB.
    pub database_url: String,
    /// People Chain RPC endpoint (quota reads; Phase 2 writer submissions).
    pub people_rpc_url: String,
    /// Network literal stamped into rows and responses.
    pub network: Network,
    /// Inviter SS58 address whose on-chain `AvailableInvites` funds tickets
    /// (required; no default).
    pub inviter_address: String,
    /// Verify-only JWT key material (required; no default — fail closed).
    pub jwt_verifier: jwt_verify::Verifier,
    /// Budget for the synchronous on-chain quota read on the create path.
    pub quota_timeout: Duration,
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
            .field("people_rpc_url", &self.people_rpc_url)
            .field("network", &self.network)
            .field("inviter_address", &self.inviter_address)
            .field("jwt_verifier", &self.jwt_verifier)
            .field("quota_timeout", &self.quota_timeout)
            .field("rate_limit", &self.rate_limit)
            .field("rate_window", &self.rate_window)
            .finish()
    }
}

impl Config {
    /// Read and validate configuration from the environment.
    ///
    /// Fails (rather than defaulting) for `DATABASE_URL`, `PEOPLE_RPC_URL`,
    /// `PEOPLE_NETWORK`, `INVITER_ADDRESS`, and the JWT key material
    /// (`JWT_JWKS_JSON` or `JWT_ED25519_PUBLIC_KEY`).
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = required_var("DATABASE_URL")?;

        let network_raw = required_var("PEOPLE_NETWORK")?;
        let network = Network::from_str(&network_raw).map_err(|_| ConfigError::Invalid {
            key: "PEOPLE_NETWORK",
            reason: format!("expected westend2|paseo|polkadot, got {network_raw}"),
        })?;
        let people_rpc_url = required_var("PEOPLE_RPC_URL")?;

        let inviter_address = required_var("INVITER_ADDRESS")?;
        subxt::utils::AccountId32::from_str(inviter_address.trim()).map_err(|e| {
            ConfigError::Invalid {
                key: "INVITER_ADDRESS",
                reason: format!("not a valid SS58 address: {e}"),
            }
        })?;
        let inviter_address = inviter_address.trim().to_string();

        let quota_timeout_secs: u64 =
            positive("QUOTA_TIMEOUT_SECS", parse_var("QUOTA_TIMEOUT_SECS", "15")?)?;
        let rate_limit: u32 = positive("RATE_LIMIT", parse_var("RATE_LIMIT", "30")?)?;
        let rate_window_secs: u64 = positive(
            "RATE_LIMIT_WINDOW_SECS",
            parse_var("RATE_LIMIT_WINDOW_SECS", "60")?,
        )?;
        Ok(Self {
            bind_addr: parse_var("BIND_ADDR", "0.0.0.0:8080")?,
            database_url,
            people_rpc_url,
            network,
            inviter_address,
            jwt_verifier: jwt_verifier_from_env()?,
            quota_timeout: Duration::from_secs(quota_timeout_secs),
            rate_limit,
            rate_window: Duration::from_secs(rate_window_secs),
        })
    }
}
