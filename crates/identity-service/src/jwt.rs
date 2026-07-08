//! JWT issuance and JWKS publication.
//!
//! identity-service is the **sole issuer**. Tokens are asymmetric (EdDSA/Ed25519)
//! so every sibling service verifies with the public key via JWKS and can never
//! mint tokens. This replaces the current backend's shared HS256 secret.

use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Ed25519 signer plus its published key id.
#[derive(Clone)]
pub struct Jwt {
    signing: SigningKey,
    kid: String,
    issuer: String,
}

#[derive(Serialize)]
struct Header<'a> {
    alg: &'a str,
    typ: &'a str,
    kid: &'a str,
}

/// Access-token claims.
///
/// `sub` and `accountId` both carry the `0x`-hex sr25519 public key: `sub` for
/// JWT-standard consumers, `accountId` for the shape the app/spec names. `plt`
/// and `appFromOfficialStore` mirror the current backend.
#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    sub: &'a str,
    #[serde(rename = "accountId")]
    account_id: &'a str,
    #[serde(rename = "appFromOfficialStore")]
    app_from_official_store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plt: Option<&'a str>,
    iat: i64,
    exp: i64,
}

impl Jwt {
    /// Build a signer from a 32-byte Ed25519 seed.
    pub fn new(seed: &[u8; 32], issuer: String) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let pubkey = signing.verifying_key().to_bytes();
        let digest = Sha256::digest(pubkey);
        let kid = hex::encode(&digest[..8]);
        Self {
            signing,
            kid,
            issuer,
        }
    }

    /// Issue a signed access token for `account_id` (`0x`-hex sr25519 pubkey).
    pub fn issue(
        &self,
        account_id: &str,
        app_from_official_store: bool,
        platform: Option<&str>,
        ttl: Duration,
    ) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let claims = Claims {
            iss: &self.issuer,
            sub: account_id,
            account_id,
            app_from_official_store,
            plt: platform,
            iat: now,
            exp: now + ttl.as_secs() as i64,
        };
        let header = Header {
            alg: "EdDSA",
            typ: "JWT",
            kid: &self.kid,
        };
        let signing_input = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header).expect("header serializes")),
            B64.encode(serde_json::to_vec(&claims).expect("claims serialize")),
        );
        let signature = self.signing.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }

    /// The JWKS document siblings fetch to verify tokens.
    pub fn jwks(&self) -> serde_json::Value {
        let x = B64.encode(self.signing.verifying_key().to_bytes());
        serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "use": "sig",
                "alg": "EdDSA",
                "kid": self.kid,
                "x": x,
            }]
        })
    }

    /// Verify a bearer token this issuer signed and return its claims.
    ///
    /// identity-service verifies its own tokens on protected routes; siblings do
    /// the same via JWKS. Checks the EdDSA signature and `exp`.
    pub fn verify(&self, token: &str) -> Result<VerifiedClaims, JwtError> {
        let mut parts = token.split('.');
        let (header, payload, signature) = match (parts.next(), parts.next(), parts.next()) {
            (Some(h), Some(p), Some(s)) if parts.next().is_none() => (h, p, s),
            _ => return Err(JwtError::Malformed),
        };

        let sig_bytes: [u8; 64] = B64
            .decode(signature)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(JwtError::Malformed)?;
        let signing_input = format!("{header}.{payload}");
        self.signing
            .verifying_key()
            .verify(
                signing_input.as_bytes(),
                &ed25519_dalek::Signature::from_bytes(&sig_bytes),
            )
            .map_err(|_| JwtError::BadSignature)?;

        let claims: VerifiedClaims = B64
            .decode(payload)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(JwtError::Malformed)?;
        if claims.exp <= time::OffsetDateTime::now_utc().unix_timestamp() {
            return Err(JwtError::Expired);
        }
        Ok(claims)
    }
}

/// Claims read back from a verified token.
#[derive(Debug, Deserialize)]
pub struct VerifiedClaims {
    /// `0x`-hex sr25519 public key of the authenticated account.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// Expiry (unix seconds).
    pub exp: i64,
}

/// Why a bearer token was rejected.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    /// Not a well-formed `header.payload.signature` JWS.
    #[error("malformed token")]
    Malformed,
    /// Signature did not verify against the issuer key.
    #[error("bad signature")]
    BadSignature,
    /// Token is past its `exp`.
    #[error("expired token")]
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_verifiable_token_with_account_id() {
        let jwt = Jwt::new(&[7u8; 32], "polkadot-app".to_string());
        let account = "0xabcdef";
        let token = jwt.issue(account, true, Some("ios"), Duration::from_secs(3600));

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWS has three segments");

        // Signature verifies against the published key.
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes: [u8; 64] = B64.decode(parts[2]).unwrap().try_into().unwrap();
        let vk = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        vk.verify(
            signing_input.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_bytes),
        )
        .expect("signature verifies");

        // Claims carry accountId and an expiry.
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["accountId"], account);
        assert_eq!(claims["sub"], account);
        assert_eq!(claims["appFromOfficialStore"], true);
        assert!(claims["exp"].as_i64().unwrap() > claims["iat"].as_i64().unwrap());

        // Header advertises EdDSA + the JWKS kid.
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "EdDSA");
        assert_eq!(header["kid"], jwt.jwks()["keys"][0]["kid"]);
    }
}
