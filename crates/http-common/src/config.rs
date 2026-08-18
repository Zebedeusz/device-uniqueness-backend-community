//! Fail-fast environment parsing helpers shared by the service configs.
//!
//! A missing or malformed required value stops the process loudly at boot
//! instead of serving with half-initialised state (same posture as
//! device-attestation). Each service keeps its own `Config` struct (and its
//! redacting `Debug`); these are only the parsing primitives.

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

/// Read a required env var, trimming and rejecting empty values.
pub fn required_var(key: &'static str) -> Result<String, ConfigError> {
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
pub fn parse_var<T>(key: &'static str, default: &str) -> Result<T, ConfigError>
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

/// Reject a numeric value equal to its type's default (i.e. zero).
pub fn positive<T>(key: &'static str, value: T) -> Result<T, ConfigError>
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

/// Load verify-only key material: `JWT_JWKS_JSON` (an inline JWKS document, as
/// served by device-attestation at `/.well-known/jwks.json`) wins; otherwise
/// `JWT_ED25519_PUBLIC_KEY` (32-byte hex, optional `0x`) pins a single key.
pub fn jwt_verifier_from_env() -> Result<jwt_verify::Verifier, ConfigError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_operational_values() {
        assert!(matches!(
            positive("RATE_LIMIT", 0_u64),
            Err(ConfigError::Invalid { key: "RATE_LIMIT", reason })
                if reason == "must be at least 1"
        ));
        assert!(positive("RATE_LIMIT", 30_u64).is_ok());
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
