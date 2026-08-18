//! People Chain RPC client (subxt online).
//!
//! Backs the username availability reads (`Resources::UsernameOwnerOf`) and
//! exposes the underlying online client ([`ChainClient::online`]) that the
//! device-attestation-chain-writer submits registrations through. On-device availability and the
//! standalone indexer are later work. Lite usernames are stored as `{base}.{NN}`
//! (two-digit discriminator `00`..`99`), so availability for a base is "which of
//! its 100 discriminators are unowned".

use std::collections::BTreeSet;
use std::collections::HashMap;

use anyhow::Context as _;
use chain_types::people;
use chain_types::PeopleConfig;
use subxt::config::RpcConfigFor;
use subxt::OnlineClient;
use subxt_rpcs::methods::legacy::StorageChangeSet;
use subxt_rpcs::{LegacyRpcMethods, RpcClient};

/// Number of discriminators per base (`00`..`99`).
const DISCRIMINATORS: u8 = 100;

/// Username whose key the health probe reads. Any well-formed key works: the
/// probe asserts that the node answers `state_queryStorageAt`, not what it
/// answers.
const HEALTH_PROBE_USERNAME: &str = "readyz-probe.00";

/// A `state_queryStorageAt` response that does not answer exactly the requested
/// keys at the requested block.
///
/// Availability fails closed: an unexpected response is an error, never "these
/// discriminators are free", because handing out a taken name yields a
/// registration that can only fail on chain.
#[derive(Debug, thiserror::Error)]
pub enum BatchReadError {
    /// Not exactly one change set.
    #[error("expected one storage change set for one block, got {0}")]
    ChangeSetCount(usize),
    /// The change set is for a block other than the pinned one.
    #[error("storage answered for block {got}, not the requested {want}")]
    BlockMismatch {
        /// The block the read was pinned to.
        want: String,
        /// The block the node answered for.
        got: String,
    },
    /// A key that was not requested.
    #[error("storage answered with a key that was not requested")]
    UnknownKey,
    /// The same key twice, so the outcome would depend on order.
    #[error("storage answered twice for the same key")]
    DuplicateKey,
    /// Unanswered keys: unknown state, not free.
    #[error("storage left {0} of the requested keys unanswered")]
    MissingKeys(usize),
}

/// The `Resources::UsernameOwnerOf` map key for a username's raw bytes.
fn owner_key(
    username: impl AsRef<[u8]>,
) -> people::runtime_types::bounded_collections::bounded_vec::BoundedVec<u8> {
    people::runtime_types::bounded_collections::bounded_vec::BoundedVec(username.as_ref().to_vec())
}

/// Resolve a `state_queryStorageAt` response into the taken discriminators.
///
/// `keys[i]` is the key for discriminator `i`. Matched by key bytes because the
/// RPC promises no ordering; every key must be answered exactly once.
fn taken_from_changes<H: PartialEq + std::fmt::Debug>(
    keys: &[Vec<u8>],
    want_block: &H,
    changes: Vec<StorageChangeSet<H>>,
) -> Result<BTreeSet<u8>, BatchReadError> {
    let [set] = <[_; 1]>::try_from(changes)
        .map_err(|changes: Vec<_>| BatchReadError::ChangeSetCount(changes.len()))?;
    if &set.block != want_block {
        return Err(BatchReadError::BlockMismatch {
            want: format!("{want_block:?}"),
            got: format!("{:?}", set.block),
        });
    }

    let index: HashMap<&[u8], usize> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| (key.as_slice(), i))
        .collect();
    let mut answered = vec![false; keys.len()];
    let mut taken = BTreeSet::new();

    for (key, value) in set.changes {
        let i = *index
            .get(key.0.as_slice())
            .ok_or(BatchReadError::UnknownKey)?;
        if std::mem::replace(&mut answered[i], true) {
            return Err(BatchReadError::DuplicateKey);
        }
        if value.is_some() {
            taken.insert(i as u8);
        }
    }

    let unanswered = answered.iter().filter(|seen| !**seen).count();
    if unanswered > 0 {
        return Err(BatchReadError::MissingKeys(unanswered));
    }
    Ok(taken)
}

/// Connected People Chain client (cheap to clone; shares one connection).
#[derive(Clone)]
pub struct ChainClient {
    client: OnlineClient<PeopleConfig>,
    /// Raw RPC over the same connection, for the multi-key storage read.
    rpc: LegacyRpcMethods<RpcConfigFor<PeopleConfig>>,
}

impl ChainClient {
    /// Connect to the People Chain RPC (auto-reconnecting) and load its metadata.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let (client, rpc) = chain_client::connect_with_rpc(url).await?;
        Ok(Self::from_parts(client, rpc))
    }

    /// Wrap an already-constructed online client and the RPC client it was
    /// built on (offline replay tests).
    pub fn from_parts(client: OnlineClient<PeopleConfig>, rpc: RpcClient) -> Self {
        Self {
            client,
            rpc: LegacyRpcMethods::new(rpc),
        }
    }

    /// Liveness probe used by `/readyz`.
    ///
    /// Covers both halves of the connection: the online client and the legacy
    /// `state_queryStorageAt` method the availability read depends on. A node
    /// can serve the modern API while not exposing the legacy one, which would
    /// leave `/readyz` green while every availability read fails.
    pub async fn health(&self) -> anyhow::Result<()> {
        let at = self
            .client
            .at_current_block()
            .await
            .context("People Chain unreachable")?;
        let key = at
            .storage()
            .entry(people::storage().resources().username_owner_of())?
            .fetch_key((owner_key(HEALTH_PROBE_USERNAME),))?;
        self.rpc
            .state_query_storage_at([key.as_slice()], Some(at.block_hash()))
            .await
            .context("People Chain does not serve state_queryStorageAt")?;
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
    /// Reads all 100 `{base}.{NN}` keys against one block snapshot in ONE
    /// `state_queryStorageAt` request. subxt's typed storage API fetches one key
    /// per request, so that path costs 100 requests per name checked and slows
    /// down under concurrent callers; the keys are the same typed encoding
    /// either way.
    ///
    /// Requires an endpoint serving the legacy `state_queryStorageAt` method.
    /// An unexpected response is a [`BatchReadError`], never a partial answer.
    pub async fn taken_discriminators(&self, base: &str) -> anyhow::Result<BTreeSet<u8>> {
        let at = self.client.at_current_block().await?;
        self.taken_discriminators_at(base, &at).await
    }

    /// [`Self::taken_discriminators`] against an already-selected block, so a
    /// caller comparing read paths can pin both to the same snapshot.
    pub async fn taken_discriminators_at(
        &self,
        base: &str,
        at: &subxt::client::ClientAtBlock<
            PeopleConfig,
            subxt::client::OnlineClientAtBlockImpl<PeopleConfig>,
        >,
    ) -> anyhow::Result<BTreeSet<u8>> {
        let block_hash = at.block_hash();
        let entry = at
            .storage()
            .entry(people::storage().resources().username_owner_of())?;

        let keys = (0..DISCRIMINATORS)
            .map(|discriminator| {
                entry.fetch_key((owner_key(format!("{base}.{discriminator:02}")),))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let changes = self
            .rpc
            .state_query_storage_at(keys.iter().map(Vec::as_slice), Some(block_hash))
            .await
            .context("reading username owners")?;

        Ok(taken_from_changes(&keys, &block_hash, changes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<Vec<u8>> {
        vec![vec![0xa0], vec![0xb0], vec![0xc0]]
    }

    fn set(block: u8, changes: &[(u8, bool)]) -> Vec<StorageChangeSet<u8>> {
        vec![StorageChangeSet {
            block,
            changes: changes
                .iter()
                .map(|(key, owned)| {
                    (
                        subxt_rpcs::methods::legacy::Bytes(vec![*key]),
                        owned.then(|| subxt_rpcs::methods::legacy::Bytes(vec![1])),
                    )
                })
                .collect(),
        }]
    }

    #[test]
    fn resolves_owned_keys_in_any_order() {
        let taken = taken_from_changes(
            &keys(),
            &7,
            set(7, &[(0xc0, true), (0xa0, false), (0xb0, true)]),
        )
        .expect("well-formed response");
        assert_eq!(taken, BTreeSet::from([1, 2]));
    }

    #[test]
    fn absent_values_are_free() {
        let taken = taken_from_changes(
            &keys(),
            &7,
            set(7, &[(0xa0, false), (0xb0, false), (0xc0, false)]),
        )
        .expect("well-formed response");
        assert!(taken.is_empty());
    }

    #[test]
    fn missing_key_is_an_error() {
        let error = taken_from_changes(&keys(), &7, set(7, &[(0xa0, true), (0xb0, false)]))
            .expect_err("short response");
        assert!(matches!(error, BatchReadError::MissingKeys(1)), "{error:?}");
    }

    #[test]
    fn unknown_key_is_an_error() {
        let error = taken_from_changes(
            &keys(),
            &7,
            set(
                7,
                &[(0xa0, true), (0xb0, false), (0xc0, false), (0xd0, false)],
            ),
        )
        .expect_err("unrequested key");
        assert!(matches!(error, BatchReadError::UnknownKey), "{error:?}");
    }

    #[test]
    fn duplicate_key_is_an_error() {
        let error = taken_from_changes(
            &keys(),
            &7,
            set(
                7,
                &[(0xa0, true), (0xa0, false), (0xb0, false), (0xc0, false)],
            ),
        )
        .expect_err("duplicate key");
        assert!(matches!(error, BatchReadError::DuplicateKey), "{error:?}");
    }

    #[test]
    fn other_block_is_an_error() {
        let error = taken_from_changes(
            &keys(),
            &7,
            set(9, &[(0xa0, false), (0xb0, false), (0xc0, false)]),
        )
        .expect_err("wrong block");
        assert!(
            matches!(error, BatchReadError::BlockMismatch { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn multiple_change_sets_are_an_error() {
        let mut changes = set(7, &[(0xa0, false), (0xb0, false), (0xc0, false)]);
        changes.extend(set(8, &[(0xa0, true)]));
        let error = taken_from_changes(&keys(), &7, changes).expect_err("two change sets");
        assert!(
            matches!(error, BatchReadError::ChangeSetCount(2)),
            "{error:?}"
        );
    }

    #[test]
    fn empty_change_set_is_an_error() {
        let error = taken_from_changes(&keys(), &7, set(7, &[])).expect_err("empty response");
        assert!(matches!(error, BatchReadError::MissingKeys(3)), "{error:?}");
    }
}
