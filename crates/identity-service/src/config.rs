//! Runtime configuration for identity-service, loaded and validated from the
//! environment at startup.
//!
//! Loading is fail-fast: [`Config::from_env`] returns an error (rather than
//! serving with half-initialised state) when a required secret is missing or
//! malformed. This is the 2026-01-28 outage lesson — a bad/absent env value
//! must stop the process loudly, not crash-loop or half-start.

use std::net::SocketAddr;
use std::time::Duration;

use secrecy::{SecretBox, SecretString};

/// identity-service runtime configuration.
///
/// `auth_enabled` and `enforce_auth` mirror the current backend's attestation
/// gates so the same binary runs attestation-off on testnets and hard-gated in
/// prod:
/// - `auth_enabled = false` makes all platform attestation a no-op (a JWT is
///   still issued; the sr25519 account proof is still verified).
/// - `enforce_auth = false` is the soft/advisory mode (log the attestation
///   verdict, do not reject); `true` hard-rejects bad or missing attestation.
#[derive(Debug)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Postgres connection string (required; no default). May embed a password,
    /// so it is a secret.
    pub database_url: SecretString,
    /// Ed25519 seed used to sign JWTs (32 bytes). identity-service is the sole issuer.
    pub jwt_secret: SecretBox<[u8; 32]>,
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
    /// Whether registration accepts the optional `dotns` block (mirrors the
    /// legacy `DOTNS_GATEWAY_ENABLED` gate and its captured 400 message).
    pub dotns_gateway_enabled: bool,
    /// Max age, in seconds, of `dotns.signedAt` (the intake freshness bound).
    pub dotns_intake_freshness_max_age_secs: u32,
    /// Max future skew, in seconds, tolerated on `dotns.signedAt`.
    pub dotns_max_future_skew_secs: u32,
    /// App IDs (`TEAMID.bundle.id`) accepted in App Attest rpIdHash checks.
    /// Required (non-empty) when `auth_enabled` is true.
    pub apple_app_attest_app_ids: Vec<String>,
    /// iOS bundle ids accepted in the `Auth-iOS-Package` header.
    /// Required (non-empty) when `auth_enabled` is true.
    pub ios_package_names: Vec<String>,
    /// Android package names accepted in key-attestation / Play Integrity
    /// verdicts. Required (non-empty) when `auth_enabled` is true.
    pub android_package_names: Vec<String>,
    /// SHA-256 fingerprint of the Play Store signing certificate.
    /// Required when `auth_enabled` is true.
    pub android_signing_digest_playstore: Option<[u8; 32]>,
    /// SHA-256 fingerprint of the website/vanilla-APK signing certificate.
    /// Required when `auth_enabled` is true.
    pub android_signing_digest_website: Option<[u8; 32]>,
    /// URL of Google's Android attestation revocation list.
    pub android_crl_url: String,
    /// Cache TTL for the attestation revocation list.
    pub android_crl_cache_ttl: Duration,
    /// Maximum age a cached CRL snapshot may reach while refreshes fail before
    /// it is refused (surfaced as a hard-mode `503`). Bounds the "serve stale"
    /// fallback to the spec's 1-hour maximum staleness (SC-004).
    pub android_crl_max_stale: Duration,
    /// Play Integrity verdict acceptance mode (`strict` default).
    pub play_integrity_mode: crate::auth::play_integrity::PlayIntegrityMode,
    /// AES-256 Play Integrity response decryption key (self-managed keys).
    /// Optional until the Play Console key exchange lands; without it the
    /// Google-API fallback is used (if configured), else every play-integrity
    /// verdict fails (posture-gated).
    pub play_integrity_decryption_key: Option<SecretBox<[u8; 32]>>,
    /// EC P-256 Play Integrity verification key, DER SPKI (self-managed keys).
    pub play_integrity_verification_key: Option<Vec<u8>>,
    /// Legacy `GOOGLE_CREDENTIALS` service account (base64 JSON) for the
    /// temporary `decodeIntegrityToken` fallback. Delete with the fallback.
    pub google_credentials: Option<crate::auth::play_integrity::google::GoogleCredentials>,
    /// Apple DeviceCheck (iOS uniqueness at username claim). `None` while
    /// `DEVICE_CHECK_IOS_ENABLED=false`; required key material otherwise.
    pub device_check: Option<DeviceCheckConfig>,
}

/// Apple DeviceCheck key material and endpoint (legacy env names).
#[derive(Debug, Clone)]
pub struct DeviceCheckConfig {
    /// Apple Developer team id (`APPLE_TEAM_ID`) — the JWT issuer.
    pub team_id: String,
    /// DeviceCheck key id (`DEVICE_CHECK_KEY_ID`) — the JWT `kid`.
    pub key_id: String,
    /// DeviceCheck `.p8` private key PEM (`DEVICE_CHECK_PRIVATE_KEY`).
    pub private_key_pem: SecretString,
    /// API base URL (`DEVICE_CHECK_URL`); production Apple by default.
    pub base_url: String,
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

        let auth_enabled = env_bool("AUTH_ENABLED", false)?;
        let apple_app_attest_app_ids = env_list("APPLE_APP_ATTEST_APP_IDS");
        let ios_package_names = env_list("IOS_PACKAGE_NAMES");
        let android_package_names = env_list("ANDROID_PACKAGE_NAMES");
        let android_signing_digest_playstore =
            parse_signing_digest("ANDROID_SIGNING_DIGEST_PLAYSTORE")?;
        let android_signing_digest_website =
            parse_signing_digest("ANDROID_SIGNING_DIGEST_WEBSITE")?;
        let play_integrity_decryption_key = parse_play_integrity_decryption_key()?;
        let play_integrity_verification_key = parse_play_integrity_verification_key()?;
        // The self-managed keys only work as a pair; exactly one set is a
        // deployment error that would silently re-route verification.
        if play_integrity_decryption_key.is_some() != play_integrity_verification_key.is_some() {
            return Err(ConfigError::Invalid {
                key: "PLAY_INTEGRITY_DECRYPTION_KEY",
                reason: "PLAY_INTEGRITY_DECRYPTION_KEY and PLAY_INTEGRITY_VERIFICATION_KEY \
                         must be set together"
                    .to_string(),
            });
        }
        // 2026-01-28 lesson: attestation-on with no verifiable App ID must stop
        // the process loudly, not silently reject every iOS device.
        if auth_enabled && apple_app_attest_app_ids.is_empty() {
            return Err(ConfigError::Missing("APPLE_APP_ATTEST_APP_IDS"));
        }
        if auth_enabled && ios_package_names.is_empty() {
            return Err(ConfigError::Missing("IOS_PACKAGE_NAMES"));
        }
        // Same rule for Android: without these every key-attestation verdict
        // fails its packageName / signing-digest check.
        if auth_enabled && android_package_names.is_empty() {
            return Err(ConfigError::Missing("ANDROID_PACKAGE_NAMES"));
        }
        if auth_enabled && android_signing_digest_playstore.is_none() {
            return Err(ConfigError::Missing("ANDROID_SIGNING_DIGEST_PLAYSTORE"));
        }
        if auth_enabled && android_signing_digest_website.is_none() {
            return Err(ConfigError::Missing("ANDROID_SIGNING_DIGEST_WEBSITE"));
        }

        Ok(Self {
            bind_addr,
            database_url: SecretString::from(database_url),
            jwt_secret: SecretBox::new(Box::new(jwt_secret)),
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "polkadot-app".to_string()),
            people_rpc_url: std::env::var("PEOPLE_RPC_URL")
                .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string()),
            attester_public_key: parse_attester_public_key()?,
            access_ttl: Duration::from_secs(parse_var("ACCESS_TOKEN_TTL_SECS", "86400")?),
            refresh_ttl: Duration::from_secs(parse_var("REFRESH_TOKEN_TTL_SECS", "2592000")?),
            challenge_ttl: Duration::from_secs(parse_var("CHALLENGE_TTL_SECS", "300")?),
            auth_rate_limit: parse_var("AUTH_RATE_LIMIT", "30")?,
            auth_rate_window: Duration::from_secs(parse_var("AUTH_RATE_WINDOW_SECS", "60")?),
            auth_enabled,
            enforce_auth: env_bool("ENFORCE_AUTH", false)?,
            dotns_gateway_enabled: env_bool("DOTNS_GATEWAY_ENABLED", true)?,
            dotns_intake_freshness_max_age_secs: parse_var(
                "DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS",
                "600",
            )?,
            dotns_max_future_skew_secs: parse_var("DOTNS_MAX_FUTURE_SKEW_SECS", "600")?,
            apple_app_attest_app_ids,
            ios_package_names,
            android_package_names,
            android_signing_digest_playstore,
            android_signing_digest_website,
            android_crl_url: std::env::var("ANDROID_ATTESTATION_CRL_URL").unwrap_or_else(|_| {
                "https://android.googleapis.com/attestation/status".to_string()
            }),
            android_crl_cache_ttl: Duration::from_secs(parse_var(
                "ANDROID_ATTESTATION_CRL_CACHE_TTL_SECS",
                "3600",
            )?),
            android_crl_max_stale: Duration::from_secs(parse_var(
                "ANDROID_ATTESTATION_CRL_MAX_STALE_SECS",
                "3600",
            )?),
            play_integrity_mode: parse_var("PLAY_INTEGRITY_MODE", "strict")?,
            play_integrity_decryption_key: play_integrity_decryption_key
                .map(|k| SecretBox::new(Box::new(k))),
            play_integrity_verification_key,
            google_credentials: parse_google_credentials()?,
            device_check: parse_device_check()?,
        })
    }

    /// A fully-populated config for unit tests (no environment access).
    #[doc(hidden)]
    pub fn test_default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
            database_url: SecretString::from("unused".to_string()),
            jwt_secret: SecretBox::new(Box::new([7u8; 32])),
            jwt_issuer: "polkadot-app".to_string(),
            people_rpc_url: "unused".to_string(),
            attester_public_key: format!("0x{}", "aa".repeat(32)),
            access_ttl: Duration::from_secs(86_400),
            refresh_ttl: Duration::from_secs(2_592_000),
            challenge_ttl: Duration::from_secs(300),
            auth_rate_limit: 1_000,
            auth_rate_window: Duration::from_secs(60),
            auth_enabled: false,
            enforce_auth: false,
            dotns_gateway_enabled: true,
            dotns_intake_freshness_max_age_secs: 600,
            dotns_max_future_skew_secs: 600,
            apple_app_attest_app_ids: Vec::new(),
            ios_package_names: Vec::new(),
            android_package_names: Vec::new(),
            android_signing_digest_playstore: None,
            android_signing_digest_website: None,
            android_crl_url: "https://android.googleapis.com/attestation/status".to_string(),
            android_crl_cache_ttl: Duration::from_secs(3_600),
            android_crl_max_stale: Duration::from_secs(3_600),
            play_integrity_mode: crate::auth::play_integrity::PlayIntegrityMode::Strict,
            play_integrity_decryption_key: None,
            play_integrity_verification_key: None,
            google_credentials: None,
            device_check: None,
        }
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

/// Parse an optional Android signing-certificate SHA-256 fingerprint env var.
///
/// Accepts the colon-separated uppercase form published in Play Console
/// (`5A:A3:…`) as well as bare hex; normalises to 32 raw bytes.
fn parse_signing_digest(key: &'static str) -> Result<Option<[u8; 32]>, ConfigError> {
    let Ok(raw) = std::env::var(key) else {
        return Ok(None);
    };
    let normalized = raw.trim().to_ascii_lowercase().replace(':', "");
    if normalized.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(&normalized).map_err(|e| ConfigError::Invalid {
        key,
        reason: format!("expected hex: {e}"),
    })?;
    let digest: [u8; 32] = bytes.try_into().map_err(|_| ConfigError::Invalid {
        key,
        reason: "expected a 32-byte SHA-256 fingerprint".to_string(),
    })?;
    Ok(Some(digest))
}

/// Parse the optional base64 AES-256 `PLAY_INTEGRITY_DECRYPTION_KEY`.
fn parse_play_integrity_decryption_key() -> Result<Option<[u8; 32]>, ConfigError> {
    use base64::Engine as _;

    const KEY: &str = "PLAY_INTEGRITY_DECRYPTION_KEY";
    let Ok(raw) = std::env::var(KEY) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| ConfigError::Invalid {
            key: KEY,
            reason: format!("expected base64: {e}"),
        })?;
    let key = bytes.try_into().map_err(|_| ConfigError::Invalid {
        key: KEY,
        reason: "expected a 32-byte AES-256 key".to_string(),
    })?;
    Ok(Some(key))
}

/// Parse the optional base64 DER-SPKI `PLAY_INTEGRITY_VERIFICATION_KEY`,
/// validating it decodes to an EC P-256 public key at startup.
fn parse_play_integrity_verification_key() -> Result<Option<Vec<u8>>, ConfigError> {
    use base64::Engine as _;
    use p256::pkcs8::DecodePublicKey as _;

    const KEY: &str = "PLAY_INTEGRITY_VERIFICATION_KEY";
    let Ok(raw) = std::env::var(KEY) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| ConfigError::Invalid {
            key: KEY,
            reason: format!("expected base64: {e}"),
        })?;
    p256::ecdsa::VerifyingKey::from_public_key_der(&der).map_err(|e| ConfigError::Invalid {
        key: KEY,
        reason: format!("expected DER SPKI EC P-256 public key: {e}"),
    })?;
    Ok(Some(der))
}

/// Parse the optional legacy `GOOGLE_CREDENTIALS` (base64 service-account
/// JSON) for the temporary Play Integrity Google-API fallback.
fn parse_google_credentials(
) -> Result<Option<crate::auth::play_integrity::google::GoogleCredentials>, ConfigError> {
    const KEY: &str = "GOOGLE_CREDENTIALS";
    let Ok(raw) = std::env::var(KEY) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    crate::auth::play_integrity::google::GoogleCredentials::parse(&raw)
        .map(Some)
        .map_err(|reason| ConfigError::Invalid { key: KEY, reason })
}

/// Parse the DeviceCheck block: `None` while `DEVICE_CHECK_IOS_ENABLED` is
/// false; enabled, the legacy vars are required and the key must be a usable
/// EC PEM (fail-fast).
fn parse_device_check() -> Result<Option<DeviceCheckConfig>, ConfigError> {
    if !env_bool("DEVICE_CHECK_IOS_ENABLED", false)? {
        return Ok(None);
    }
    let require = |key: &'static str| -> Result<String, ConfigError> {
        std::env::var(key)
            .map(|v| v.trim().to_string())
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::Missing(key))
    };
    // Tolerate `\n`-escaped PEMs from single-line env injection.
    let private_key_pem = require("DEVICE_CHECK_PRIVATE_KEY")?.replace("\\n", "\n");
    jsonwebtoken::EncodingKey::from_ec_pem(private_key_pem.as_bytes()).map_err(|e| {
        ConfigError::Invalid {
            key: "DEVICE_CHECK_PRIVATE_KEY",
            reason: format!("not a usable EC private key PEM: {e}"),
        }
    })?;
    Ok(Some(DeviceCheckConfig {
        team_id: require("APPLE_TEAM_ID")?,
        key_id: require("DEVICE_CHECK_KEY_ID")?,
        private_key_pem: SecretString::from(private_key_pem),
        base_url: std::env::var("DEVICE_CHECK_URL")
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .unwrap_or_else(|_| "https://api.devicecheck.apple.com/v1".to_string()),
    }))
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

/// Strict boolean env parsing. A missing variable takes `default`; a present
/// value must be one of the accepted tokens (a case-insensitive superset of the
/// legacy Effect `Config.boolean` values). An unrecognized value aborts startup
/// rather than silently disabling a security gate (e.g. `AUTH_ENABLED=treu`).
fn env_bool(key: &'static str, default: bool) -> Result<bool, ConfigError> {
    let Ok(raw) = std::env::var(key) else {
        return Ok(default);
    };
    parse_bool(raw.trim()).ok_or_else(|| ConfigError::Invalid {
        key,
        reason: format!("expected a boolean (true/false), got {:?}", raw.trim()),
    })
}

/// The accepted boolean tokens (case-insensitive). `None` = unrecognized, which
/// the caller turns into a startup error.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "y" => Some(true),
        "false" | "no" | "off" | "0" | "n" => Some(false),
        _ => None,
    }
}

/// Parse a comma-separated env var into trimmed, non-empty entries.
fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_secrets() {
        let mut config = Config::test_default();
        config.database_url =
            SecretString::from("postgres://user:SUPER_SECRET_PW@localhost/db".to_string());
        config.play_integrity_decryption_key = Some(SecretBox::new(Box::new([9u8; 32])));
        config.device_check = Some(DeviceCheckConfig {
            team_id: "TEAM123".to_string(),
            key_id: "KEY123".to_string(),
            private_key_pem: SecretString::from("BEGIN P8_SECRET_MATERIAL END".to_string()),
            base_url: "https://api.devicecheck.apple.com".to_string(),
        });

        let rendered = format!("{config:?}");

        // Secret material must never reach the Debug output.
        assert!(!rendered.contains("SUPER_SECRET_PW"), "database URL leaked");
        assert!(
            !rendered.contains("P8_SECRET_MATERIAL"),
            "DeviceCheck key leaked"
        );
        // secrecy's opaque Debug renders `[REDACTED]`; non-secret fields still show.
        assert!(rendered.contains("REDACTED"));
        assert!(rendered.contains("TEAM123"));

        // The nested DeviceCheckConfig self-redacts on its own too.
        let dc = format!("{:?}", config.device_check.unwrap());
        assert!(!dc.contains("P8_SECRET_MATERIAL"));
        assert!(dc.contains("REDACTED"));
    }

    #[test]
    fn parse_bool_is_strict() {
        for t in ["true", "TRUE", "yes", "on", "1", "y"] {
            assert_eq!(parse_bool(t), Some(true), "{t:?} should be true");
        }
        for f in ["false", "No", "off", "0", "n"] {
            assert_eq!(parse_bool(f), Some(false), "{f:?} should be false");
        }
        // Unrecognized tokens are rejected (env_bool turns this into a startup
        // error) rather than silently defaulting to false.
        for bad in ["treu", "tru", "", "2", "enabled", "t"] {
            assert_eq!(parse_bool(bad), None, "{bad:?} should be rejected");
        }
    }
}
