//! The chain-writer's signing key.

use std::str::FromStr as _;

use anyhow::Context as _;
use chain_types::PeopleConfig;
use subxt::utils::{AccountId32, MultiSignature};

/// Signing key for the writer.
///
/// Supports two secret forms so the same binary works for dev and prod:
/// - a `SecretUri` (dev `//Alice`, a BIP-39 mnemonic, or a `0x`+64-hex 32-byte
///   seed), handled by `subxt-signer`; and
/// - a `0x`+128-hex raw 64-byte sr25519 secret (the polkadot.js / sp-core
///   "expanded" secret the legacy backend stored), which `subxt-signer` cannot
///   ingest through its public API. This is the format the shared attester key
///   uses, so we build a `schnorrkel` keypair from it directly.
pub enum WriterSigner {
    Uri(subxt_signer::sr25519::Keypair),
    Raw(schnorrkel::Keypair),
}

impl WriterSigner {
    /// Build a signer from a configured secret (`CHAIN_WRITER_SIGNER_SURI`).
    pub fn from_secret(secret: &str) -> anyhow::Result<Self> {
        let trimmed = secret.trim();
        // A `0x`+128-hex value is a raw 64-byte sr25519 secret (polkadot.js
        // format); subxt-signer only accepts a 32-byte seed or a mnemonic, so
        // build the keypair from `schnorrkel` directly here.
        if let Some(hex) = trimmed.strip_prefix("0x") {
            if hex.len() == 128 {
                let bytes = hex::decode(hex).context("decoding raw sr25519 secret key")?;
                let secret = schnorrkel::SecretKey::from_ed25519_bytes(&bytes)
                    .map_err(|e| anyhow::anyhow!("invalid 64-byte sr25519 secret key: {e}"))?;
                let keypair = schnorrkel::Keypair {
                    public: secret.to_public(),
                    secret,
                };
                return Ok(Self::Raw(keypair));
            }
        }
        let uri = subxt_signer::SecretUri::from_str(trimmed)
            .context("parsing CHAIN_WRITER_SIGNER_SURI")?;
        let keypair = subxt_signer::sr25519::Keypair::from_uri(&uri)
            .context("building signer keypair from SURI")?;
        Ok(Self::Uri(keypair))
    }

    /// The 32-byte sr25519 public key (the on-chain `AccountId32`).
    pub fn public_bytes(&self) -> [u8; 32] {
        match self {
            Self::Uri(k) => k.public_key().0,
            Self::Raw(k) => k.public.to_bytes(),
        }
    }

    /// Sign with the substrate signing context, matching `subxt-signer` and sp-core.
    fn sign_bytes(&self, payload: &[u8]) -> [u8; 64] {
        match self {
            Self::Uri(k) => k.sign(payload).0,
            Self::Raw(k) => {
                let context = schnorrkel::signing_context(b"substrate");
                k.sign(context.bytes(payload)).to_bytes()
            }
        }
    }
}

impl subxt::tx::Signer<PeopleConfig> for WriterSigner {
    fn account_id(&self) -> AccountId32 {
        AccountId32(self.public_bytes())
    }

    fn sign(&self, signer_payload: &[u8]) -> MultiSignature {
        MultiSignature::Sr25519(self.sign_bytes(signer_payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schnorrkel::{ExpansionMode, MiniSecretKey};

    /// `//Alice` resolves through the `SecretUri` path to the well-known dev account.
    #[test]
    fn uri_path_derives_dev_alice() {
        let signer = WriterSigner::from_secret("//Alice").expect("valid dev uri");
        // //Alice's sr25519 public key.
        let expected =
            hex::decode("d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d")
                .unwrap();
        assert_eq!(signer.public_bytes().as_slice(), expected.as_slice());
    }

    /// The same account, whether loaded from its 32-byte seed (subxt-signer path)
    /// or its expanded 64-byte secret (the polkadot.js format we build directly),
    /// derives the identical public key. The 64-byte form is the one subxt-signer
    /// cannot ingest through its public API.
    #[test]
    fn raw_64_byte_secret_matches_seed_path() {
        let seed = [7u8; 32];
        let keypair = MiniSecretKey::from_bytes(&seed)
            .unwrap()
            .expand_to_keypair(ExpansionMode::Ed25519);
        let raw64 = keypair.secret.to_ed25519_bytes();

        let from_seed =
            WriterSigner::from_secret(&format!("0x{}", hex::encode(seed))).expect("valid seed");
        let from_raw =
            WriterSigner::from_secret(&format!("0x{}", hex::encode(raw64))).expect("valid raw");

        assert!(matches!(from_seed, WriterSigner::Uri(_)));
        assert!(matches!(from_raw, WriterSigner::Raw(_)));
        assert_eq!(from_seed.public_bytes(), keypair.public.to_bytes());
        assert_eq!(from_raw.public_bytes(), keypair.public.to_bytes());
    }

    /// The raw path signs a payload that verifies under the substrate context.
    #[test]
    fn raw_signature_verifies() {
        let seed = [9u8; 32];
        let keypair = MiniSecretKey::from_bytes(&seed)
            .unwrap()
            .expand_to_keypair(ExpansionMode::Ed25519);
        let raw64 = keypair.secret.to_ed25519_bytes();
        let signer =
            WriterSigner::from_secret(&format!("0x{}", hex::encode(raw64))).expect("valid raw");

        let message = b"identity-backend-v2";
        let sig = signer.sign_bytes(message);
        let public = schnorrkel::PublicKey::from_bytes(&signer.public_bytes()).unwrap();
        let signature = schnorrkel::Signature::from_bytes(&sig).unwrap();
        assert!(public
            .verify_simple(b"substrate", message, &signature)
            .is_ok());
    }
}
