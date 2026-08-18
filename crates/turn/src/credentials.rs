//! coturn REST-API ephemeral credential minting (the construction the legacy
//! backend and coturn's `--use-auth-secret` mode share):
//!
//! - JWT issuance uses `username = "{now + ttl}:{hex(8 random bytes)}"`;
//! - proof issuance uses a deterministic 16-byte id bound to
//!   `(product, contextual alias, expiry = now + ttl)`;
//! - `password = base64(HMAC_<algo>(secret, username))`.
//!
//! The TURN relay recomputes the same HMAC from its copy of the secret, so
//! nothing is stored anywhere — cryptographic parity with the legacy issuer is
//! pinned in `tests/legacy_wire.rs` by recomputing passwords over captured
//! usernames.

use std::str::FromStr;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use hmac::{Hmac, Mac as _};

/// HMAC hash algorithm for the password (legacy `TURN_AUTH_ALGORITHM`).
/// Default SHA1 — what coturn's REST API mode historically expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// HMAC-SHA1 (the coturn default).
    Sha1,
    /// HMAC-SHA256.
    Sha256,
    /// HMAC-SHA384.
    Sha384,
    /// HMAC-SHA512.
    Sha512,
}

impl Algorithm {
    /// The legacy env literal for this algorithm.
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha384 => "SHA384",
            Algorithm::Sha512 => "SHA512",
        }
    }
}

impl FromStr for Algorithm {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SHA1" => Ok(Algorithm::Sha1),
            "SHA256" => Ok(Algorithm::Sha256),
            "SHA384" => Ok(Algorithm::Sha384),
            "SHA512" => Ok(Algorithm::Sha512),
            _ => Err(()),
        }
    }
}

/// A minted credential pair, ready for the wire.
#[derive(Clone)]
pub struct Credentials {
    /// `"{unixExpiry}:{hexId}"`.
    pub username: String,
    /// Base64 HMAC over `username`.
    pub password: String,
}

/// The credential minter: the shared secret, algorithm, and TTL.
pub struct Issuer {
    secret: Vec<u8>,
    algorithm: Algorithm,
    ttl_secs: u64,
}

/// The HMAC key must never reach logs or error output.
impl std::fmt::Debug for Issuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issuer")
            .field("secret", &"<redacted>")
            .field("algorithm", &self.algorithm)
            .field("ttl_secs", &self.ttl_secs)
            .finish()
    }
}

impl Issuer {
    /// Build an issuer over the raw (base64-decoded) shared secret.
    pub fn new(secret: Vec<u8>, algorithm: Algorithm, ttl_secs: u64) -> Self {
        Self {
            secret,
            algorithm,
            ttl_secs,
        }
    }

    /// Mint a credential pair for `id`, expiring `ttl` after `now_unix`.
    pub fn issue(&self, now_unix: u64, id: [u8; 8]) -> Credentials {
        self.issue_until(now_unix + self.ttl_secs, &id)
    }

    /// Mint a proof credential for a person and product, expiring `ttl_secs`
    /// after `now_unix`.
    pub fn issue_for_proof(&self, now_unix: u64, product_id: &str, alias: &[u8]) -> Credentials {
        let expiry = now_unix + self.ttl_secs;
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&self.secret)
            .expect("HMAC accepts any key length");
        mac.update(b"dub/turn-proof-credential-id/v1\0");
        mac.update(
            &u64::try_from(product_id.len())
                .expect("product id length fits u64")
                .to_be_bytes(),
        );
        mac.update(product_id.as_bytes());
        mac.update(
            &u64::try_from(alias.len())
                .expect("alias length fits u64")
                .to_be_bytes(),
        );
        mac.update(alias);
        mac.update(&expiry.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        self.issue_until(expiry, &digest[..16])
    }

    fn issue_until(&self, expiry: u64, id: &[u8]) -> Credentials {
        let username = format!("{}:{}", expiry, hex::encode(id));
        let password = self.password_for(&username);
        Credentials { username, password }
    }

    /// The base64 HMAC for a username (also the relay-side verification — and
    /// how the fixture replay proves parity with the legacy issuer).
    pub fn password_for(&self, username: &str) -> String {
        macro_rules! hmac_bytes {
            ($digest:ty) => {{
                let mut mac = Hmac::<$digest>::new_from_slice(&self.secret)
                    .expect("HMAC accepts any key length");
                mac.update(username.as_bytes());
                mac.finalize().into_bytes().to_vec()
            }};
        }

        let bytes = match self.algorithm {
            Algorithm::Sha1 => hmac_bytes!(sha1::Sha1),
            Algorithm::Sha256 => hmac_bytes!(sha2::Sha256),
            Algorithm::Sha384 => hmac_bytes!(sha2::Sha384),
            Algorithm::Sha512 => hmac_bytes!(sha2::Sha512),
        };
        BASE64.encode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_is_expiry_colon_hex_id() {
        let issuer = Issuer::new(vec![0x42; 32], Algorithm::Sha1, 1800);
        let credentials = issuer.issue(1_784_757_652 - 1800, [0x0a; 8]);
        assert_eq!(credentials.username, "1784757652:0a0a0a0a0a0a0a0a");
    }

    #[test]
    fn password_is_the_hmac_over_the_username() {
        // RFC 2202 HMAC-SHA1 test case 2: key "Jefe", data "what do ya want
        // for nothing?" → 0xeffcdf6ae5eb2fa2d27416d5f184df9c259a7c79.
        let issuer = Issuer::new(b"Jefe".to_vec(), Algorithm::Sha1, 0);
        assert_eq!(
            issuer.password_for("what do ya want for nothing?"),
            BASE64.encode(
                hex::decode("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79").expect("valid hex")
            ),
        );
    }

    #[test]
    fn proof_credentials_expire_after_full_ttl() {
        let issuer = Issuer::new(vec![0x42; 32], Algorithm::Sha1, 60);
        let first = issuer.issue_for_proof(121, "game.dot", &[7; 32]);
        assert!(first.username.starts_with("181:"));
        assert_eq!(first.username.split(':').nth(1).expect("id").len(), 32);

        let second = issuer.issue_for_proof(150, "game.dot", &[7; 32]);
        assert!(second.username.starts_with("210:"));
    }

    #[test]
    fn proof_credentials_change_across_product_alias_or_time() {
        let issuer = Issuer::new(vec![0x42; 32], Algorithm::Sha1, 60);
        let issue = |now, product, alias| issuer.issue_for_proof(now, product, alias).username;
        let base = issue(121, "game.dot", &[7; 32]);

        assert_ne!(base, issue(121, "other.dot", &[7; 32]));
        assert_ne!(base, issue(121, "game.dot", &[8; 32]));
        assert_ne!(base, issue(122, "game.dot", &[7; 32]));
    }

    #[test]
    fn each_algorithm_yields_its_digest_length() {
        for (algorithm, digest_len) in [
            (Algorithm::Sha1, 20),
            (Algorithm::Sha256, 32),
            (Algorithm::Sha384, 48),
            (Algorithm::Sha512, 64),
        ] {
            let issuer = Issuer::new(vec![1; 16], algorithm, 60);
            let password = issuer.password_for("100:abcd");
            let decoded = BASE64.decode(password).expect("valid base64");
            assert_eq!(decoded.len(), digest_len, "{algorithm:?}");
        }
    }

    #[test]
    fn algorithm_parses_the_legacy_literals_only() {
        assert_eq!(Algorithm::from_str("SHA1"), Ok(Algorithm::Sha1));
        assert_eq!(Algorithm::from_str("SHA512"), Ok(Algorithm::Sha512));
        assert!(Algorithm::from_str("sha1").is_err());
        assert!(Algorithm::from_str("MD5").is_err());
    }
}
