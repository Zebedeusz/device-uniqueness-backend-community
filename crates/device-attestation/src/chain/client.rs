//! People Chain RPC client (subxt online).
//!
//! Backs the username availability reads (`Resources::UsernameOwnerOf`) and
//! exposes the underlying online client ([`ChainClient::online`]) that the
//! device-attestation-chain-writer submits registrations through. On-device availability and the
//! standalone indexer are later work. Lite usernames are stored as `{base}.{NN}`
//! (two-digit discriminator `00`..`99`), so availability for a base is "which of
//! its 100 discriminators are unowned".

use std::collections::BTreeSet;

use anyhow::Context as _;
use chain_types::people;
use chain_types::PeopleConfig;
use futures::future::try_join_all;
use subxt::OnlineClient;

/// Number of discriminators per base (`00`..`99`).
const DISCRIMINATORS: u8 = 100;

/// The `Resources::UsernameOwnerOf` map key for a username's raw bytes.
fn owner_key(
    username: impl AsRef<[u8]>,
) -> people::runtime_types::bounded_collections::bounded_vec::BoundedVec<u8> {
    people::runtime_types::bounded_collections::bounded_vec::BoundedVec(username.as_ref().to_vec())
}

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

    /// Wrap an already-constructed online client (offline replay tests).
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

    /// The underlying online client, for the device-attestation-chain-writer's tx submission.
    pub fn online(&self) -> &OnlineClient<PeopleConfig> {
        &self.client
    }

    /// The current owner of a full lite username (`base.digits`), if any.
    ///
    /// Used by the device-attestation-chain-writer to reconcile: an unknown submit outcome is
    /// resolved by reading `UsernameOwnerOf` rather than blindly resubmitting.
    pub async fn username_owner(&self, full_username: &str) -> anyhow::Result<Option<[u8; 32]>> {
        let at = self.client.at_current_block().await?;
        let address = people::storage().resources().username_owner_of();
        let owner = at
            .storage()
            .try_fetch(address, (owner_key(full_username),))
            .await?;
        match owner {
            Some(value) => Ok(Some(value.decode()?.0)),
            None => Ok(None),
        }
    }

    /// The free balance of `account`, in planck (`0` for a non-existent account).
    ///
    /// Backs the registration queue's priority groups: the subject's balance
    /// maps to a queue group at intake and on every advancer iteration.
    pub async fn free_balance(&self, account: [u8; 32]) -> anyhow::Result<u128> {
        let at = self.client.at_current_block().await?;
        let address = people::storage().system().account();
        let info = at
            .storage()
            .try_fetch(address, (subxt::utils::AccountId32(account),))
            .await?;
        match info {
            Some(value) => Ok(value.decode()?.data.free),
            None => Ok(0),
        }
    }

    /// The attestation allowance left for `account` (`0` when it has none).
    ///
    /// `PeopleLite::AttestationAllowance` is what the attester spends per
    /// registration: at zero, every `attest` fails and registration stops. With
    /// proxy signing the allowance belongs to the authority (`ATTESTER_ACCOUNT`),
    /// not the signing key.
    pub async fn attestation_allowance(&self, account: [u8; 32]) -> anyhow::Result<u32> {
        let at = self.client.at_current_block().await?;
        let address = people::storage().people_lite().attestation_allowance();
        let allowance = at
            .storage()
            .try_fetch(address, (subxt::utils::AccountId32(account),))
            .await?;
        match allowance {
            Some(value) => Ok(value.decode()?),
            None => Ok(0),
        }
    }

    /// Return the set of discriminators (`0`..`99`) already owned for `base`.
    ///
    /// Reads all 100 `{base}.{NN}` keys against one block snapshot.
    pub async fn taken_discriminators(&self, base: &str) -> anyhow::Result<BTreeSet<u8>> {
        let at = self.client.at_current_block().await?;
        let storage = at.storage();
        let storage = &storage;

        let lookups = (0..DISCRIMINATORS).map(move |discriminator| async move {
            let username = format!("{base}.{discriminator:02}");
            let address = people::storage().resources().username_owner_of();
            let owned = storage
                .try_fetch(address, (owner_key(username),))
                .await?
                .is_some();
            anyhow::Ok((discriminator, owned))
        });

        let results = try_join_all(lookups).await?;
        Ok(results
            .into_iter()
            .filter_map(|(discriminator, owned)| owned.then_some(discriminator))
            .collect())
    }
}
