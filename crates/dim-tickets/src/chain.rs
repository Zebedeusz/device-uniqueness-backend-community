//! People Chain RPC client for dim-tickets.
//!
//! The api bin uses the read path (the inviter's `AvailableInvites` quota,
//! checked synchronously on the create route); the writer submits
//! `Utility.force_batch` batches of `set_invite_ticket` calls, optionally
//! wrapped in `Proxy.proxy` of a cold inviter primary.

use anyhow::Context as _;
use chain_client::WriterSigner;
use chain_types::people;
use chain_types::people::runtime_types::next_people_paseo_runtime::{ProxyType, RuntimeCall};
use chain_types::people::runtime_types::{
    indiv_pallet_game, indiv_pallet_proof_of_ink, pallet_utility,
};
use chain_types::{PeopleConfig, PeopleExtrinsicParamsBuilder};
use std::str::FromStr as _;
use subxt::tx::TransactionStatus;
use subxt::utils::{AccountId32, MultiAddress};
use subxt::OnlineClient;

use crate::tickets::Dim;

/// Connected People Chain client (cheap to clone; shares one connection).
#[derive(Clone)]
pub struct ChainClient {
    client: OnlineClient<PeopleConfig>,
}

impl ChainClient {
    /// Connect to the People Chain RPC (auto-reconnecting) and load its metadata.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: chain_client::connect(url).await?,
        })
    }

    /// Wrap an already-built [`OnlineClient`]. Production always goes through
    /// [`Self::connect`]; this exists so tests can assemble a client offline
    /// (vendored metadata + a dead mock backend).
    pub fn from_online(client: OnlineClient<PeopleConfig>) -> Self {
        Self { client }
    }

    /// Liveness probe used by `/readyz`.
    pub async fn health(&self) -> anyhow::Result<()> {
        self.client
            .at_current_block()
            .await
            .context("People Chain unreachable")?;
        Ok(())
    }

    /// The underlying online client, for the Phase 2 writer's tx submission.
    pub fn online(&self) -> &OnlineClient<PeopleConfig> {
        &self.client
    }

    /// The inviter's remaining `AvailableInvites` for `dim` (0 when the key is
    /// absent — an unfunded inviter has no invites).
    pub async fn available_invites(&self, dim: Dim, inviter: &str) -> anyhow::Result<u32> {
        let inviter = subxt::utils::AccountId32::from_str(inviter)
            .map_err(|e| anyhow::anyhow!("invalid inviter SS58 address: {e}"))?;
        let at = self.client.at_current_block().await?;
        let available = match dim {
            Dim::Game => {
                let address = people::storage().game().available_invites();
                at.storage()
                    .try_fetch(address, (inviter,))
                    .await?
                    .map(|value| value.decode())
                    .transpose()?
            }
            Dim::ProofOfInk => {
                let address = people::storage().proof_of_ink().available_invites();
                at.storage()
                    .try_fetch(address, (inviter,))
                    .await?
                    .map(|value| value.decode())
                    .transpose()?
            }
        };
        Ok(available.unwrap_or(0))
    }

    /// Sign and submit one `Utility.force_batch` of `set_invite_ticket` calls
    /// and wait for finalization, returning the ordered per-item outcomes.
    ///
    /// `Utility.force_batch` keeps executing after an inner-call failure, so
    /// the extrinsic itself succeeds and per-item results arrive as ordered
    /// `Utility.ItemCompleted` / `Utility.ItemFailed` events.
    pub async fn submit_invite_batch(
        &self,
        tickets: &[(AccountId32, Dim)],
        signer: &WriterSigner,
        proxy_for: Option<&AccountId32>,
    ) -> anyhow::Result<FinalizedBatch> {
        let calls: Vec<RuntimeCall> = tickets
            .iter()
            .map(|(ticket, dim)| set_invite_ticket_call(*dim, *ticket))
            .collect();
        // Nonce comes from the chain at sign time: this writer submits one
        // extrinsic at a time, so there is no in-flight lane to track locally.
        let params = PeopleExtrinsicParamsBuilder::<PeopleConfig>::new().build();
        let mut tx_client = self.client.tx().await?;

        // The proxy wrap changes the payload's static type, so sign per branch.
        let signed = match proxy_for {
            Some(real) => {
                let inner =
                    RuntimeCall::Utility(pallet_utility::pallet::Call::force_batch { calls });
                let payload = people::tx().proxy().proxy(
                    MultiAddress::Id(*real),
                    // Legacy signed with force_proxy_type = Any.
                    Some(ProxyType::Any),
                    inner,
                );
                tx_client.create_signed(&payload, signer, params).await?
            }
            None => {
                let payload = people::tx().utility().force_batch(calls);
                tx_client.create_signed(&payload, signer, params).await?
            }
        };
        let tx_hash = format!("{:?}", signed.hash());
        tracing::info!(batch = tickets.len(), tx = %tx_hash, "submitting invite batch");

        let mut progress = signed.submit_and_watch().await?;
        let wait = async {
            loop {
                match progress.next().await {
                    Some(status) => match status? {
                        TransactionStatus::InFinalizedBlock(in_block) => {
                            let events = in_block.wait_for_success().await?;
                            let items = chain_client::batch_item_outcomes(
                                events.iter().filter_map(|event| event.ok()).map(|event| {
                                    (
                                        event.pallet_name().to_string(),
                                        event.event_name().to_string(),
                                    )
                                }),
                            );
                            let block_hash = format!("{:?}", in_block.block_hash());
                            return anyhow::Ok((items, in_block.block_hash(), block_hash));
                        }
                        TransactionStatus::Error { message } => {
                            anyhow::bail!("tx error: {message}")
                        }
                        TransactionStatus::Invalid { message } => {
                            anyhow::bail!("tx invalid: {message}")
                        }
                        TransactionStatus::Dropped { message } => {
                            anyhow::bail!("tx dropped: {message}")
                        }
                        _ => {}
                    },
                    None => anyhow::bail!("tx status stream ended before finalization"),
                }
            }
        };
        let (items, hash, block_hash) = wait.await?;

        let block_number = self
            .client
            .at_block(hash)
            .await
            .context("fetching finalized block header")?
            .block_number();

        Ok(FinalizedBatch {
            items,
            block_hash,
            block_number,
        })
    }
}

/// A finalized `force_batch`: ordered item outcomes plus its block coordinates.
#[derive(Debug)]
pub struct FinalizedBatch {
    /// Ordered `ItemCompleted` (`true`) / `ItemFailed` (`false`) events;
    /// index = inner-call index.
    pub items: Vec<bool>,
    /// Finalized block hash (`0x…`).
    pub block_hash: String,
    /// Finalized block number.
    pub block_number: u64,
}

/// One `Game.set_invite_ticket` / `ProofOfInk.set_invite_ticket` inner call.
fn set_invite_ticket_call(dim: Dim, ticket: AccountId32) -> RuntimeCall {
    match dim {
        Dim::Game => {
            RuntimeCall::Game(indiv_pallet_game::pallet::Call::set_invite_ticket { ticket })
        }
        Dim::ProofOfInk => {
            RuntimeCall::ProofOfInk(indiv_pallet_proof_of_ink::pallet::Call::set_invite_ticket {
                ticket,
            })
        }
    }
}
