//! Asset Hub RPC client (subxt online). The dotNS gateway lane's transport.
//!
//! The counterpart to [`super::client::ChainClient`]. It serves the *other*
//! chain the `device-attestation-chain-writer` submits to.
//!
//! Everything here is built **dynamically by name** rather than from vendored
//! codegen. The writer needs one extrinsic (`DotnsGateway::reserve_name`), two
//! storage maps and two constants. The runtime behind them is the
//! fastest-churning one in the individuality family. See
//! `docs/plans/active/dotns-gateway-write.md` for the trade-off.
//!
//! The two live Asset Hubs run *different* `reserve_name` extrinsics.
//! [`AssetHubClient::connect`] therefore asserts the connected runtime's
//! argument names against [`crate::dotns::RESERVE_NAME_FIELDS`]. On a mismatch
//! it refuses to return a client. A mis-shaped call is never encoded, let alone
//! submitted.

use anyhow::Context as _;
use chain_types::AssetHubConfig;
use subxt::dynamic::{At as _, Value};
use subxt::utils::AccountId32;
use subxt::OnlineClient;

use crate::dotns;

/// The gateway pallet's name on Asset Hub.
const PALLET: &str = "DotnsGateway";

/// The freshness bounds `reserve_name` enforces.
///
/// Read from the chain rather than configured. The backend cannot re-sign. A
/// drifted local copy either wastes an extrinsic or discards a still-valid
/// reservation.
#[derive(Debug, Clone, Copy)]
pub struct ValidityWindow {
    /// `MaxValiditySeconds`: a signature is accepted while
    /// `now <= signed_at + this`.
    pub max_validity_secs: u64,
    /// `MaxFutureSkewSeconds`: a signature is accepted while
    /// `signed_at <= now + this`.
    pub max_future_skew_secs: u64,
}

/// Connected Asset Hub client. Cheap to clone. Clones share one connection.
#[derive(Clone)]
pub struct AssetHubClient {
    client: OnlineClient<AssetHubConfig>,
}

impl AssetHubClient {
    /// Connects to the Asset Hub RPC with an auto-reconnecting transport.
    ///
    /// Loads the runtime's metadata. Verifies it runs the `reserve_name` this
    /// writer builds.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = chain_client::connect_asset_hub(url).await?;
        let this = Self { client };
        this.check_reserve_name_shape()
            .await
            .with_context(|| format!("Asset Hub at {url}"))?;
        Ok(this)
    }

    /// Wraps an already-constructed online client. For offline replay tests.
    pub fn from_online(client: OnlineClient<AssetHubConfig>) -> Self {
        Self { client }
    }

    /// The underlying online client. Used for the writer's tx submission.
    pub fn online(&self) -> &OnlineClient<AssetHubConfig> {
        &self.client
    }

    /// Liveness probe.
    pub async fn health(&self) -> anyhow::Result<()> {
        self.client
            .at_current_block()
            .await
            .context("Asset Hub unreachable")?;
        Ok(())
    }

    /// Compares the connected runtime's `reserve_name` argument names against
    /// the shape this writer encodes.
    async fn check_reserve_name_shape(&self) -> anyhow::Result<()> {
        let metadata = self.client.at_current_block().await?.metadata();
        let pallet = metadata
            .pallet_by_name(PALLET)
            .with_context(|| format!("runtime declares no {PALLET} pallet"))?;
        let call = pallet
            .call_variant_by_name("reserve_name")
            .with_context(|| format!("{PALLET} declares no reserve_name call"))?;
        let fields: Vec<&str> = call
            .fields
            .iter()
            .map(|field| field.name.as_deref().unwrap_or("<unnamed>"))
            .collect();
        dotns::check_reserve_name_shape(&fields)?;
        Ok(())
    }

    /// The account that reserved `lite_label`, if any.
    ///
    /// Backs the idempotency pre-check and the `SUBMITTING` reconcile. An
    /// unknown submit outcome is resolved by reading `LiteLabelOwner`. Nothing
    /// is ever blindly resubmitted.
    pub async fn lite_label_owner(&self, lite_label: &str) -> anyhow::Result<Option<[u8; 32]>> {
        let at = self.client.at_current_block().await?;
        let address = subxt::dynamic::storage::<_, AccountId32>(PALLET, "LiteLabelOwner");
        let owner = at
            .storage()
            .try_fetch(address, (label_key(lite_label),))
            .await?;
        match owner {
            Some(value) => Ok(Some(value.decode()?.0)),
            None => Ok(None),
        }
    }

    /// The gateway attestation allowance left for `account`. `0` when it has
    /// none.
    ///
    /// Separate from People's `PeopleLite::AttestationAllowance`. They are two
    /// budgets on two chains. Each is spent one per registration.
    pub async fn attestation_allowance(&self, account: [u8; 32]) -> anyhow::Result<u32> {
        let at = self.client.at_current_block().await?;
        let address = subxt::dynamic::storage::<_, u32>(PALLET, "AttestationAllowance");
        let allowance = at
            .storage()
            .try_fetch(address, (AccountId32(account),))
            .await?;
        match allowance {
            Some(value) => Ok(value.decode()?),
            None => Ok(0),
        }
    }

    /// The signer's free balance on Asset Hub, in planck.
    ///
    /// `reserve_name` is `Pays::No` only when it succeeds. A run of failures is
    /// paid here.
    pub async fn free_balance(&self, account: [u8; 32]) -> anyhow::Result<u128> {
        let at = self.client.at_current_block().await?;
        let address = subxt::dynamic::storage::<_, Value>("System", "Account");
        let info = at
            .storage()
            .try_fetch(address, (AccountId32(account),))
            .await?;
        let Some(info) = info else { return Ok(0) };
        let value = info.decode()?;
        let free = value
            .at("data")
            .and_then(|data| data.at("free"))
            .and_then(|free| free.as_u128())
            .context("System::Account has no data.free field")?;
        Ok(free)
    }

    /// The pallet's freshness constants.
    pub async fn validity_window(&self) -> anyhow::Result<ValidityWindow> {
        let at = self.client.at_current_block().await?;
        let constants = at.constants();
        Ok(ValidityWindow {
            max_validity_secs: constants
                .entry(subxt::dynamic::constant::<u64>(
                    PALLET,
                    "MaxValiditySeconds",
                ))
                .context("reading DotnsGateway::MaxValiditySeconds")?,
            max_future_skew_secs: constants
                .entry(subxt::dynamic::constant::<u64>(
                    PALLET,
                    "MaxFutureSkewSeconds",
                ))
                .context("reading DotnsGateway::MaxFutureSkewSeconds")?,
        })
    }
}

/// The `LiteLabelOwner` map key: `BaseLabel(BoundedVec<u8, 32>)`. It encodes as
/// a plain length-prefixed byte string.
fn label_key(lite_label: &str) -> Value {
    Value::from_bytes(lite_label.as_bytes())
}
