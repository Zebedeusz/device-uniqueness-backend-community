//! Verify-only JWT validation — the cross-service auth contract.
//!
//! device-attestation is the **sole issuer** of asymmetric (EdDSA/Ed25519) access
//! tokens and publishes its public key as JWKS at `/.well-known/jwks.json`.
//! Sibling services (notifications, invite-tickets, …) gate their authenticated
//! routes with this crate: JWKS/public-key loading, Ed25519 signature
//! verification, and claims validation. No signing key ever enters this crate —
//! a consumer can verify tokens but can never mint them.
//!
//! Verification is offline once key material is loaded: no runtime call to
//! device-attestation is made per request.

use base64::Engine as _;
use ed25519_dalek::{Verifier as _, VerifyingKey};
use serde::Deserialize;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Verifies bearer tokens issued by device-attestation against its public keys.
#[derive(Clone, Debug)]
pub struct Verifier {
    /// Published keys; `kid` is `None` only for pinned keys loaded without one.
    keys: Vec<(Option<String>, VerifyingKey)>,
}

/// Claims read back from a verified token.
///
/// `accountId` is the shape the app/spec names; it duplicates `sub` on the
/// wire, so only `accountId` is decoded here.
#[derive(Debug, Deserialize)]
pub struct VerifiedClaims {
    /// `0x`-hex sr25519 public key of the authenticated account.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// Expiry (unix seconds).
    pub exp: i64,
    /// Client platform (`"ios"`/`"android"`) from the token's `plt` claim,
    /// set server-side at attestation. Absent when the issuer omitted it.
    #[serde(rename = "plt", default)]
    pub platform: Option<String>,
    /// Whether attestation judged the app an official-store install
    /// (`appFromOfficialStore` claim, set server-side from real attestation
    /// evidence). `None` when the issuer omitted it (old tokens, no-op
    /// posture) — consumers choose their own default per posture.
    #[serde(rename = "appFromOfficialStore", default)]
    pub app_from_official_store: Option<bool>,
}

#[derive(Deserialize)]
struct TokenHeader {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
struct JwksDoc {
    keys: Vec<JwksKey>,
}

#[derive(Deserialize)]
struct JwksKey {
    kty: String,
    crv: Option<String>,
    kid: Option<String>,
    x: Option<String>,
}

/// Why JWKS/public-key material could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    /// Not a parseable JWKS document.
    #[error("malformed JWKS document")]
    Malformed,
    /// The document parsed but contained no usable Ed25519 key.
    #[error("no usable Ed25519 key in JWKS document")]
    NoUsableKey,
}

/// Why a bearer token was rejected.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    /// Not a well-formed `header.payload.signature` JWS with decodable JSON.
    #[error("malformed token")]
    Malformed,
    /// Header `alg` is not `EdDSA` (rejected before any signature check).
    #[error("unsupported algorithm")]
    UnsupportedAlgorithm,
    /// Token names a `kid` this verifier has no key for.
    #[error("unknown key id")]
    UnknownKey,
    /// Signature did not verify against any candidate issuer key.
    #[error("bad signature")]
    BadSignature,
    /// Token is past its `exp`.
    #[error("expired token")]
    Expired,
}

impl Verifier {
    /// Load keys from a JWKS document (the body of `/.well-known/jwks.json`).
    ///
    /// Keys that are not `OKP`/`Ed25519` or carry undecodable material are
    /// skipped; an empty usable set is [`JwksError::NoUsableKey`].
    pub fn from_jwks(jwks_json: &str) -> Result<Self, JwksError> {
        let doc: JwksDoc = serde_json::from_str(jwks_json).map_err(|_| JwksError::Malformed)?;
        let keys: Vec<(Option<String>, VerifyingKey)> = doc
            .keys
            .into_iter()
            .filter(|k| k.kty == "OKP" && k.crv.as_deref() == Some("Ed25519"))
            .filter_map(|k| {
                let bytes: [u8; 32] = B64.decode(k.x?).ok()?.try_into().ok()?;
                Some((k.kid, VerifyingKey::from_bytes(&bytes).ok()?))
            })
            .collect();
        if keys.is_empty() {
            return Err(JwksError::NoUsableKey);
        }
        Ok(Self { keys })
    }

    /// Build a verifier around one pinned public key (no JWKS fetch).
    ///
    /// Tokens naming any `kid` still verify — a pinned key has no id to
    /// mismatch. device-attestation uses this to verify its own tokens.
    pub fn from_verifying_key(key: VerifyingKey) -> Self {
        Self {
            keys: vec![(None, key)],
        }
    }

    /// Verify a bearer token and return its claims.
    ///
    /// Checks (in order): JWS shape, `alg == EdDSA`, `kid` known (when both the
    /// token and the keys carry one), Ed25519 signature, `exp` in the future.
    pub fn verify(&self, token: &str) -> Result<VerifiedClaims, JwtError> {
        self.verify_at(token, time::OffsetDateTime::now_utc().unix_timestamp())
    }

    /// [`Self::verify`] against a supplied clock (unix seconds).
    pub fn verify_at(&self, token: &str, now_unix: i64) -> Result<VerifiedClaims, JwtError> {
        let mut parts = token.split('.');
        let (header_b64, payload_b64, signature_b64) =
            match (parts.next(), parts.next(), parts.next()) {
                (Some(h), Some(p), Some(s)) if parts.next().is_none() => (h, p, s),
                _ => return Err(JwtError::Malformed),
            };

        let header: TokenHeader = B64
            .decode(header_b64)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(JwtError::Malformed)?;
        if header.alg != "EdDSA" {
            return Err(JwtError::UnsupportedAlgorithm);
        }

        let candidates: Vec<&VerifyingKey> = match &header.kid {
            Some(kid) => {
                let matched: Vec<&VerifyingKey> = self
                    .keys
                    .iter()
                    .filter(|(key_id, _)| key_id.as_deref().is_none_or(|id| id == kid))
                    .map(|(_, key)| key)
                    .collect();
                if matched.is_empty() {
                    return Err(JwtError::UnknownKey);
                }
                matched
            }
            None => self.keys.iter().map(|(_, key)| key).collect(),
        };

        let sig_bytes: [u8; 64] = B64
            .decode(signature_b64)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(JwtError::Malformed)?;
        let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        let signing_input = format!("{header_b64}.{payload_b64}");
        if !candidates
            .iter()
            .any(|key| key.verify(signing_input.as_bytes(), &signature).is_ok())
        {
            return Err(JwtError::BadSignature);
        }

        let claims: VerifiedClaims = B64
            .decode(payload_b64)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(JwtError::Malformed)?;
        if claims.exp <= now_unix {
            return Err(JwtError::Expired);
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn issue(seed: &[u8; 32], kid: Option<&str>, alg: &str, exp: i64) -> String {
        let signing = SigningKey::from_bytes(seed);
        let header = match kid {
            Some(kid) => format!(r#"{{"alg":"{alg}","typ":"JWT","kid":"{kid}"}}"#),
            None => format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#),
        };
        let claims = format!(r#"{{"accountId":"0xabcdef","iat":{NOW},"exp":{exp}}}"#);
        let signing_input = format!("{}.{}", B64.encode(header), B64.encode(claims));
        let signature = signing.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }

    fn jwks_for(seed: &[u8; 32], kid: &str) -> String {
        let x = B64.encode(SigningKey::from_bytes(seed).verifying_key().to_bytes());
        format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","use":"sig","alg":"EdDSA","kid":"{kid}","x":"{x}"}}]}}"#
        )
    }

    #[test]
    fn verifies_token_against_jwks_keys() {
        let seed = [7u8; 32];
        let kid = "issuer-kid";
        let verifier = Verifier::from_jwks(&jwks_for(&seed, kid)).unwrap();

        let token = issue(&seed, Some(kid), "EdDSA", NOW + 3600);
        let claims = verifier.verify_at(&token, NOW).unwrap();
        assert_eq!(claims.account_id, "0xabcdef");
        assert_eq!(claims.exp, NOW + 3600);
        // Optional attestation claims decode as absent when the issuer
        // omitted them (old tokens stay verifiable).
        assert_eq!(claims.platform, None);
        assert_eq!(claims.app_from_official_store, None);
    }

    #[test]
    fn decodes_the_optional_attestation_claims() {
        let seed = [7u8; 32];
        let kid = "issuer-kid";
        let verifier = Verifier::from_jwks(&jwks_for(&seed, kid)).unwrap();

        let signing = SigningKey::from_bytes(&seed);
        let header = format!(r#"{{"alg":"EdDSA","typ":"JWT","kid":"{kid}"}}"#);
        let exp = NOW + 3600;
        let claims = format!(
            r#"{{"accountId":"0xabcdef","plt":"android","appFromOfficialStore":false,"exp":{exp}}}"#
        );
        let signing_input = format!("{}.{}", B64.encode(header), B64.encode(claims));
        let signature = signing.sign(signing_input.as_bytes());
        let token = format!("{signing_input}.{}", B64.encode(signature.to_bytes()));

        let claims = verifier.verify_at(&token, NOW).unwrap();
        assert_eq!(claims.platform.as_deref(), Some("android"));
        assert_eq!(claims.app_from_official_store, Some(false));
    }

    #[test]
    fn verifies_token_without_kid_against_pinned_key() {
        let seed = [9u8; 32];
        let verifier = Verifier::from_verifying_key(SigningKey::from_bytes(&seed).verifying_key());
        let token = issue(&seed, None, "EdDSA", NOW + 60);
        assert!(verifier.verify_at(&token, NOW).is_ok());
    }

    #[test]
    fn rejects_non_eddsa_alg_before_signature_check() {
        let seed = [7u8; 32];
        let verifier = Verifier::from_verifying_key(SigningKey::from_bytes(&seed).verifying_key());
        // Signed correctly, but the header claims HS256 / none.
        for alg in ["HS256", "none"] {
            let token = issue(&seed, None, alg, NOW + 3600);
            assert!(matches!(
                verifier.verify_at(&token, NOW),
                Err(JwtError::UnsupportedAlgorithm)
            ));
        }
    }

    #[test]
    fn rejects_expired_token() {
        let seed = [7u8; 32];
        let verifier = Verifier::from_verifying_key(SigningKey::from_bytes(&seed).verifying_key());
        let token = issue(&seed, None, "EdDSA", NOW - 1);
        assert!(matches!(
            verifier.verify_at(&token, NOW),
            Err(JwtError::Expired)
        ));
    }

    #[test]
    fn rejects_token_signed_by_another_key() {
        let verifier =
            Verifier::from_verifying_key(SigningKey::from_bytes(&[7u8; 32]).verifying_key());
        let token = issue(&[8u8; 32], None, "EdDSA", NOW + 3600);
        assert!(matches!(
            verifier.verify_at(&token, NOW),
            Err(JwtError::BadSignature)
        ));
    }

    #[test]
    fn rejects_unknown_kid() {
        let seed = [7u8; 32];
        let verifier = Verifier::from_jwks(&jwks_for(&seed, "known-kid")).unwrap();
        let token = issue(&seed, Some("other-kid"), "EdDSA", NOW + 3600);
        assert!(matches!(
            verifier.verify_at(&token, NOW),
            Err(JwtError::UnknownKey)
        ));
    }

    #[test]
    fn rejects_malformed_tokens() {
        let verifier =
            Verifier::from_verifying_key(SigningKey::from_bytes(&[7u8; 32]).verifying_key());
        for token in ["", "a.b", "a.b.c.d", "!!.!!.!!"] {
            assert!(matches!(
                verifier.verify_at(token, NOW),
                Err(JwtError::Malformed)
            ));
        }
    }

    #[test]
    fn rejects_jwks_without_usable_keys() {
        assert!(matches!(
            Verifier::from_jwks(r#"{"keys":[]}"#),
            Err(JwksError::NoUsableKey)
        ));
        assert!(matches!(
            Verifier::from_jwks(r#"{"keys":[{"kty":"RSA","kid":"a"}]}"#),
            Err(JwksError::NoUsableKey)
        ));
        assert!(matches!(
            Verifier::from_jwks("not json"),
            Err(JwksError::Malformed)
        ));
    }
}
