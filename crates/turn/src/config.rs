//! Runtime configuration for turn, loaded and validated from the environment
//! at startup.
//!
//! Loading is fail-fast (via the shared `http-common` config helpers): a
//! missing or malformed required value stops the process loudly at boot
//! instead of serving with half-initialised state. Env names mirror legacy
//! (`TURN_SECRET`, `TURN_AUTH_ALGORITHM`, `TURN_REALM`, `ICE_SERVERS`), with
//! one deliberate format change: `TURN_TTL_SECS` takes plain seconds where
//! legacy's `TURN_TTL` took Effect duration strings (deployment-config
//! format, not wire contract).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::str::FromStr as _;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
pub use http_common::config::ConfigError;
use http_common::config::{
    jwt_verifier_from_env, parse_renamed, parse_var, positive, required_var,
};

use crate::credentials::Algorithm;

/// Accepted clock skew, either side of server time, for a proof request's
/// timestamp.
pub const PROOF_MAX_SKEW_SECS: u64 = 60;
/// Fixed ring-root refresh cadence.
pub const PROOF_ROOT_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);
/// How stale a ring-root snapshot may be and still verify a proof.
///
/// Twenty missed refreshes. An RPC outage keeps the last snapshot so issuance
/// survives a blip, but a member removed from the ring must stop verifying in
/// bounded time, so past this the proof route fails closed with 503.
pub const PROOF_MAX_ROOT_AGE: std::time::Duration = std::time::Duration::from_secs(600);

/// turn runtime configuration.
#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Raw (base64-decoded) HMAC secret shared with the TURN relay
    /// (required; no default — fail closed).
    pub turn_secret: Vec<u8>,
    /// HMAC algorithm for the password (default SHA1, the coturn default).
    pub algorithm: Algorithm,
    /// Credential time-to-live in seconds (default 1800 = legacy's
    /// 30 minutes).
    pub ttl_secs: u64,
    /// TURN realm (required + validated like legacy; reserved — configured on
    /// the relay, never on this wire).
    pub realm: String,
    /// ICE server URLs echoed verbatim in every 201 body.
    pub ice_servers: Vec<String>,
    /// Verify-only JWT key material (required; no default — fail closed).
    pub jwt_verifier: jwt_verify::Verifier,
    /// Max requests per authenticated subject per window.
    pub rate_limit: u32,
    /// Rate-limit window.
    pub rate_window: std::time::Duration,
    /// Proof-authorized issuance (`TURN_PROOF_*`); `None` = feature off.
    pub proof: Option<ProofConfig>,
}

/// The HMAC secret must never reach logs, spans, or error output, so `Debug`
/// is implemented by hand. (The JWT material is the public verification key.)
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("turn_secret", &"<redacted>")
            .field("algorithm", &self.algorithm)
            .field("ttl_secs", &self.ttl_secs)
            .field("realm", &self.realm)
            .field("ice_servers", &self.ice_servers)
            .field("jwt_verifier", &self.jwt_verifier)
            .field("rate_limit", &self.rate_limit)
            .field("rate_window", &self.rate_window)
            .field("proof", &self.proof)
            .finish()
    }
}

impl Config {
    /// Read and validate configuration from the environment.
    ///
    /// Fails (rather than defaulting) for `TURN_SECRET`, `TURN_REALM`, and
    /// the JWT key material (`JWT_JWKS_JSON` or `JWT_ED25519_PUBLIC_KEY`).
    pub fn from_env() -> Result<Self, ConfigError> {
        let turn_secret = decode_secret(&required_var("TURN_SECRET")?)?;

        let algorithm_raw: String = parse_var("TURN_AUTH_ALGORITHM", "SHA1")?;
        let algorithm =
            Algorithm::from_str(algorithm_raw.trim()).map_err(|()| ConfigError::Invalid {
                key: "TURN_AUTH_ALGORITHM",
                reason: format!("expected SHA1|SHA256|SHA384|SHA512, got {algorithm_raw}"),
            })?;

        let ttl_secs: u64 = positive("TURN_TTL_SECS", parse_var("TURN_TTL_SECS", "1800")?)?;
        let realm = validated_realm(&required_var("TURN_REALM")?)?;
        let ice_servers = parse_ice_servers(&std::env::var("ICE_SERVERS").unwrap_or_default())?;

        let rate_limit: u32 = positive(
            "TURN_RATE_LIMIT",
            parse_renamed("TURN_RATE_LIMIT", "RATE_LIMIT", "30")?,
        )?;
        let rate_window_secs: u64 = positive(
            "TURN_RATE_LIMIT_WINDOW_SECS",
            parse_renamed(
                "TURN_RATE_LIMIT_WINDOW_SECS",
                "RATE_LIMIT_WINDOW_SECS",
                "60",
            )?,
        )?;
        let proof = ProofConfig::from_env()?;
        Ok(Self {
            bind_addr: parse_var("BIND_ADDR", "0.0.0.0:8080")?,
            turn_secret,
            algorithm,
            ttl_secs,
            realm,
            ice_servers,
            jwt_verifier: jwt_verifier_from_env()?,
            rate_limit,
            rate_window: std::time::Duration::from_secs(rate_window_secs),
            proof,
        })
    }
}

/// Configuration for proof-authorized issuance. Parsed only when
/// `TURN_PROOF_ENABLED=true`; every deployment-specific authority-bearing
/// value (RPC, genesis, products) is required with no default — fail closed.
#[derive(Clone)]
pub struct ProofConfig {
    /// People Chain RPC endpoint the root refresher connects to.
    pub rpc_url: String,
    /// Genesis hash this deployment is bound to (32-byte hex). Checked
    /// against the connected chain by the root refresher; not part of the
    /// proved message.
    pub genesis: [u8; 32],
    /// Accepted product id → the context a host derives for it. A caller
    /// proves under its own product's context — the only context shape TrUAPI
    /// can express; an unlisted product is refused before verification.
    pub contexts: BTreeMap<String, [u8; 32]>,
    /// Concurrent ring-VRF verifications. Defaults to available CPUs minus
    /// one (floor 1); saturated requests wait briefly, then fail with 503.
    pub concurrency: usize,
}

/// The contexts and identifiers are not secret, but keep `Debug` compact.
impl std::fmt::Debug for ProofConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofConfig")
            .field("rpc_url", &self.rpc_url)
            .field("genesis", &hex::encode(self.genesis))
            .field(
                "contexts",
                &self
                    .contexts
                    .iter()
                    .map(|(product, context)| format!("{product}={}", hex::encode(context)))
                    .collect::<Vec<_>>(),
            )
            .field("concurrency", &self.concurrency)
            .finish()
    }
}

impl ProofConfig {
    /// Parse the `TURN_PROOF_*` family; `Ok(None)` when the feature is off.
    fn from_env() -> Result<Option<Self>, ConfigError> {
        let enabled: bool = parse_var("TURN_PROOF_ENABLED", "false")?;
        if !enabled {
            return Ok(None);
        }
        let concurrency = proof_concurrency(
            &std::env::var("TURN_PROOF_CONCURRENCY").unwrap_or_else(|_| "auto".to_string()),
        )?;
        let suffix: u32 = parse_var("TURN_PROOF_CONTEXT_SUFFIX", "0")?;
        Ok(Some(Self {
            rpc_url: required_var("TURN_PROOF_RPC_URL")?,
            genesis: hex32("TURN_PROOF_GENESIS", &required_var("TURN_PROOF_GENESIS")?)?,
            contexts: contexts(&required_var("TURN_PROOF_PRODUCTS")?, suffix)?,
            concurrency,
        }))
    }
}

fn proof_concurrency(raw: &str) -> Result<usize, ConfigError> {
    if raw.trim().eq_ignore_ascii_case("auto") {
        return Ok(std::thread::available_parallelism()
            .map(|cpus| cpus.get().saturating_sub(1).max(1))
            .unwrap_or(1));
    }
    positive(
        "TURN_PROOF_CONCURRENCY",
        raw.trim()
            .parse::<usize>()
            .map_err(|error| ConfigError::Invalid {
                key: "TURN_PROOF_CONCURRENCY",
                reason: error.to_string(),
            })?,
    )
}

/// Derive one context per accepted product from a comma-separated list.
fn contexts(raw: &str, suffix: u32) -> Result<BTreeMap<String, [u8; 32]>, ConfigError> {
    let mut contexts = BTreeMap::new();
    for entry in raw.split(',') {
        let product = entry.trim();
        if product.is_empty() {
            return Err(ConfigError::Invalid {
                key: "TURN_PROOF_PRODUCTS",
                reason: "contains an empty product id".to_string(),
            });
        }
        contexts.insert(
            product.to_string(),
            crate::proof::context::product_context(product, suffix),
        );
    }
    if contexts.is_empty() {
        return Err(ConfigError::Invalid {
            key: "TURN_PROOF_PRODUCTS",
            reason: "must list at least one product id".to_string(),
        });
    }
    Ok(contexts)
}

/// Decode non-empty hex (with or without `0x`).
fn hex_bytes(key: &'static str, raw: &str) -> Result<Vec<u8>, ConfigError> {
    let bytes =
        hex::decode(raw.trim().trim_start_matches("0x")).map_err(|_| ConfigError::Invalid {
            key,
            reason: "invalid hex encoding".to_string(),
        })?;
    if bytes.is_empty() {
        return Err(ConfigError::Invalid {
            key,
            reason: "must not be empty".to_string(),
        });
    }
    Ok(bytes)
}

/// Decode exactly 32 hex bytes.
fn hex32(key: &'static str, raw: &str) -> Result<[u8; 32], ConfigError> {
    hex_bytes(key, raw)?
        .try_into()
        .map_err(|v: Vec<u8>| ConfigError::Invalid {
            key,
            reason: format!("expected 32 bytes, got {}", v.len()),
        })
}

/// Decode the base64 `TURN_SECRET` (legacy stored it base64-encoded too).
fn decode_secret(raw: &str) -> Result<Vec<u8>, ConfigError> {
    let bytes = BASE64
        .decode(raw.trim())
        .map_err(|_| ConfigError::Invalid {
            key: "TURN_SECRET",
            reason: "invalid base64 encoding".to_string(),
        })?;
    if bytes.is_empty() {
        return Err(ConfigError::Invalid {
            key: "TURN_SECRET",
            reason: "must not be empty".to_string(),
        });
    }
    Ok(bytes)
}

/// The legacy realm constraint: alphanumeric, underscores, dots, hyphens.
fn validated_realm(raw: &str) -> Result<String, ConfigError> {
    let realm = raw.trim();
    let valid = !realm.is_empty()
        && realm
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        return Err(ConfigError::Invalid {
            key: "TURN_REALM",
            reason: "must contain only alphanumeric characters, underscores, dots, and hyphens"
                .to_string(),
        });
    }
    Ok(realm.to_string())
}

/// Parse the comma-separated `ICE_SERVERS` list (unset/empty → empty list, as
/// legacy defaulted). Entries are echoed verbatim on the wire, so only shape
/// is checked: non-empty, with a URL scheme separator.
fn parse_ice_servers(raw: &str) -> Result<Vec<String>, ConfigError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() || !entry.contains(':') {
                return Err(ConfigError::Invalid {
                    key: "ICE_SERVERS",
                    reason: format!("invalid ICE server URL: {entry:?}"),
                });
            }
            Ok(entry.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_base64_secret_and_rejects_garbage() {
        assert_eq!(decode_secret("QUJD").expect("valid"), b"ABC");
        assert!(decode_secret("not base64!").is_err());
        assert!(decode_secret("").is_err());
    }

    #[test]
    fn realm_enforces_the_legacy_character_set() {
        assert_eq!(
            validated_realm(" example.org ").expect("valid"),
            "example.org"
        );
        assert!(validated_realm("with space").is_err());
        assert!(validated_realm("").is_err());
    }

    #[test]
    fn products_derive_one_distinct_context_each() {
        let derived = contexts(" vox.dot , game.dot ", 0).expect("valid");
        assert_eq!(
            derived.keys().collect::<Vec<_>>(),
            vec!["game.dot", "vox.dot"]
        );
        assert_ne!(derived["vox.dot"], derived["game.dot"]);
        assert_eq!(
            derived["vox.dot"],
            crate::proof::context::product_context("vox.dot", 0)
        );
        assert_ne!(
            derived["vox.dot"],
            contexts(" vox.dot ", 1).expect("valid")["vox.dot"]
        );
        assert!(contexts("vox.dot,,game.dot", 0).is_err());
        assert!(contexts("", 0).is_err());
    }

    #[test]
    fn proof_concurrency_accepts_auto_or_an_explicit_positive_limit() {
        assert!(proof_concurrency("auto").expect("available CPUs") >= 1);
        assert_eq!(proof_concurrency(" 4 ").expect("explicit limit"), 4);
        assert!(proof_concurrency("0").is_err());
        assert!(proof_concurrency("many").is_err());
    }

    #[test]
    fn ice_servers_split_trim_and_reject_schemeless_entries() {
        assert_eq!(parse_ice_servers("").expect("valid"), Vec::<String>::new());
        assert_eq!(
            parse_ice_servers(" stun:a.example:3478 , turn:b.example:3478?transport=udp ")
                .expect("valid"),
            vec![
                "stun:a.example:3478".to_string(),
                "turn:b.example:3478?transport=udp".to_string(),
            ]
        );
        assert!(parse_ice_servers("stun:a.example:3478,,").is_err());
        assert!(parse_ice_servers("no-scheme").is_err());
    }
}
