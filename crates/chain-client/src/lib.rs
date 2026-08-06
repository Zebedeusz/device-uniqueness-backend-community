//! People Chain connection + chain-writer signing key — the product-agnostic
//! transport layer shared by the services that talk to People Chain.
//!
//! Owns exactly two concerns (extracted when dim-tickets became the second
//! consumer, per the lazy-extraction rule in `docs/architecture.md`):
//! - [`connect`]: an auto-reconnecting subxt [`OnlineClient`] over the
//!   [`chain_types::PeopleConfig`] transaction-extension pipeline.
//! - [`WriterSigner`]: the sr25519 signing key a single-instance chain writer
//!   submits with.
//!
//! Product logic — which storage to read, which calls to build — stays in each
//! service; generated chain types stay in `chain-types`.

use std::str::FromStr as _;

use anyhow::Context as _;
use chain_types::PeopleConfig;
use subxt::utils::{AccountId32, MultiSignature};
use subxt::OnlineClient;
use subxt_rpcs::client::{ReconnectingRpcClient, RpcClient};

/// Connect to a People Chain RPC with an auto-reconnecting transport and load
/// its metadata.
pub async fn connect(url: &str) -> anyhow::Result<OnlineClient<PeopleConfig>> {
    OnlineClient::<PeopleConfig>::from_rpc_client(reconnecting_rpc_client(url).await?)
        .await
        .with_context(|| format!("connecting to People Chain at {url}"))
}

async fn reconnecting_rpc_client(url: &str) -> anyhow::Result<RpcClient> {
    let rpc_client = ReconnectingRpcClient::builder()
        .build(url)
        .await
        .with_context(|| format!("connecting to People Chain at {url}"))?;
    Ok(RpcClient::new(rpc_client))
}

/// Signing key for a single-instance chain writer.
///
/// Supports two secret forms so the same binary works for dev and prod:
/// - a `SecretUri` (dev `//Alice`, a BIP-39 mnemonic, or a `0x`+64-hex 32-byte
///   seed), handled by `subxt-signer`; and
/// - a `0x`+128-hex raw 64-byte sr25519 secret (the polkadot.js / sp-core
///   "expanded" secret the legacy backend stored), which `subxt-signer` cannot
///   ingest through its public API, so we build a `schnorrkel` keypair from it
///   directly.
pub enum WriterSigner {
    /// Keypair derived from a `SecretUri` by `subxt-signer`.
    Uri(subxt_signer::sr25519::Keypair),
    /// Keypair built directly from a raw 64-byte expanded secret.
    Raw(schnorrkel::Keypair),
}

impl WriterSigner {
    /// Build a signer from a configured secret.
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
        let uri =
            subxt_signer::SecretUri::from_str(trimmed).context("parsing signer secret URI")?;
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

/// Map a finalized `Utility.force_batch`'s events onto ordered per-item
/// outcomes: `Utility.ItemCompleted` → `true`, `Utility.ItemFailed` →
/// `false`, everything else ignored. Index = inner-call index (the
/// positional contract both ticket writers rely on: `force_batch` keeps
/// executing after an inner failure and emits one item event per call).
pub fn batch_item_outcomes<P, E>(events: impl IntoIterator<Item = (P, E)>) -> Vec<bool>
where
    P: AsRef<str>,
    E: AsRef<str>,
{
    events
        .into_iter()
        .filter(|(pallet, _)| pallet.as_ref() == "Utility")
        .filter_map(|(_, event)| match event.as_ref() {
            "ItemCompleted" => Some(true),
            "ItemFailed" => Some(false),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use jsonrpsee_core::server::RpcModule;
    use jsonrpsee_server::{ServerBuilder, ServerHandle};
    use schnorrkel::{ExpansionMode, MiniSecretKey};
    use subxt_rpcs::rpc_params;

    use super::*;

    #[tokio::test]
    async fn reconnecting_rpc_client_recovers_after_server_restart() -> anyhow::Result<()> {
        let (server, url) = TestRpcServer::start(None).await?;
        let rpc = reconnecting_rpc_client(&url).await?;

        let first: String = rpc.request("ping", rpc_params![]).await?;
        assert_eq!(first, "pong");

        server.stop().await?;
        let addr = url
            .strip_prefix("ws://")
            .context("test RPC URL should be ws://")?
            .parse::<SocketAddr>()?;
        let (_server, _) = TestRpcServer::start(Some(addr)).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let second = tokio::time::timeout(
            Duration::from_secs(5),
            rpc.request::<String>("ping", rpc_params![]),
        )
        .await??;
        assert_eq!(second, "pong");

        Ok(())
    }

    struct TestRpcServer {
        handle: ServerHandle,
    }

    impl TestRpcServer {
        async fn start(addr: Option<SocketAddr>) -> anyhow::Result<(Self, String)> {
            let addr = addr.unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into());
            let server = ServerBuilder::default().build(addr).await?;
            let addr = server.local_addr()?;
            let mut module = RpcModule::new(());
            module.register_async_method("ping", |_, _, _| async { "pong" })?;
            let handle = server.start(module);

            Ok((Self { handle }, format!("ws://{addr}")))
        }

        async fn stop(self) -> anyhow::Result<()> {
            self.handle.stop()?;
            self.handle.stopped().await;
            Ok(())
        }
    }

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
    /// derives the identical public key.
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

    /// Positional item mapping: only `Utility.ItemCompleted`/`ItemFailed`
    /// count, in order; foreign pallets and other Utility events (including a
    /// same-named event on another pallet) are ignored.
    #[test]
    fn batch_item_outcomes_keeps_order_and_ignores_foreign_events() {
        let events = [
            ("System", "ExtrinsicSuccess"),
            ("Utility", "ItemCompleted"),
            ("Game", "ItemCompleted"), // same name, wrong pallet
            ("Utility", "ItemFailed"),
            ("Utility", "BatchCompletedWithErrors"),
            ("Utility", "ItemCompleted"),
        ];
        assert_eq!(batch_item_outcomes(events), vec![true, false, true]);
        assert!(batch_item_outcomes(Vec::<(&str, &str)>::new()).is_empty());
    }
}
