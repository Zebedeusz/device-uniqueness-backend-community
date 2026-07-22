//! The pool maintainer: keeps each `(dim, network)` pool stocked with
//! on-chain-registered ticket keypairs.
//!
//! Per tick (legacy cadence: every 6s): count `available` → below target →
//! generate up to `batch` keypairs → submit one `set_invite_ticket`
//! `force_batch` as the inviter → on finalization insert the accepted tickets
//! as `available` — **only then** do they become claimable. Failed items and
//! failed batches discard their keypairs; nothing is retried and no
//! failed/retrying row ever exists (the legacy two-state invariant).
//!
//! Batch sizing is the legacy AIMD policy: grow by one on success up to the
//! max, halve on failure (floor 1). (Legacy distinguished resource-exhaustion
//! failures — halve — from other whole-batch faults — hold; this maintainer
//! halves on any whole-batch failure, a deliberate conservative
//! simplification of daemon-internal behavior, not wire contract.)
//!
//! Single-instance: one inviter = one nonce lane. Deploy runs exactly one
//! replica, and each tick additionally takes a Postgres advisory lock
//! (skipping the tick if another holder exists), so a deploy overlap cannot
//! double-submit.

use std::time::Duration;

use anyhow::Context as _;
use chain_client::WriterSigner;
use sqlx::PgPool;
use subxt::utils::AccountId32;

use crate::chain::ChainClient;
use crate::sign::{self, TicketKeypair};
use crate::tickets::{self, Dim, Network};

/// Advisory lock key for the tick guard (stable, service-specific).
const POOL_LOCK_KEY: i64 = 0x1417_1CE7_0001;

/// Pool maintainer tunables (legacy defaults; all env-overridable).
#[derive(Debug, Clone)]
pub struct PoolTuning {
    /// Sleep between ticks.
    pub interval: Duration,
    /// Target `available` count per pool.
    pub target: i64,
    /// Max tickets per submitted batch.
    pub batch_max: u32,
    /// Budget for one batch's submission + finalization.
    pub finalize_timeout: Duration,
}

impl Default for PoolTuning {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(6),
            target: 10_000,
            batch_max: 100,
            finalize_timeout: Duration::from_secs(60),
        }
    }
}

/// The AIMD batch-size settlement (port of the legacy `settleBatchAttempt`):
/// success grows by one up to `max`; failure halves, floor 1.
pub fn settle_batch_size(current: u32, max: u32, succeeded: bool) -> u32 {
    if succeeded {
        current.saturating_add(1).min(max)
    } else {
        (current / 2).max(1)
    }
}

/// The signing identity the maintainer submits with.
pub struct InviterIdentity {
    /// Transaction signer (the inviter itself, or its proxy delegate).
    pub signer: WriterSigner,
    /// The inviter account whose quota funds tickets — stamped into rows.
    pub inviter: AccountId32,
    /// The inviter SS58 string as configured (stamped into rows verbatim).
    pub inviter_ss58: String,
    /// When set, submissions wrap in `Proxy.proxy(real = inviter)`.
    pub proxy: bool,
}

/// One maintenance pass over one pool. Returns the number of tickets added.
///
/// Chain finalization can take minutes; no DB transaction is held across it —
/// the only DB writes are the post-finalization inserts.
pub async fn tick_pool(
    pool: &PgPool,
    chain: &ChainClient,
    identity: &InviterIdentity,
    dim: Dim,
    network: Network,
    tuning: &PoolTuning,
    batch_size: u32,
) -> anyhow::Result<(u32, u32)> {
    let available = tickets::count_available(pool, dim, network)
        .await
        .context("counting available tickets")?;
    let needed = tuning.target.saturating_sub(available);
    tracing::debug!(
        dim = dim.as_str(),
        network = network.as_str(),
        available,
        target = tuning.target,
        "pool status"
    );
    if needed <= 0 {
        return Ok((0, batch_size));
    }

    let size = u32::try_from(needed).unwrap_or(u32::MAX).min(batch_size);
    let generated: Vec<(TicketKeypair, [u8; 32])> = (0..size)
        .map(|_| {
            let seed = sign::generate_seed();
            let keypair = TicketKeypair::from_stored_secret(&seed).expect("fresh seed is valid");
            (keypair, seed)
        })
        .collect();
    let accounts: Vec<AccountId32> = generated
        .iter()
        .map(|(keypair, _)| AccountId32(keypair.public_bytes()))
        .collect();

    let proxy_for = identity.proxy.then_some(&identity.inviter);
    let finalized = tokio::time::timeout(
        tuning.finalize_timeout,
        chain.submit_ticket_batch(&accounts, dim, &identity.signer, proxy_for),
    )
    .await
    .context("batch finalization timed out")??;

    // Insert exactly the accepted items; everything else is discarded with
    // its keypair (the seeds drop with `generated`).
    let mut inserted = 0u32;
    for (index, (keypair, seed)) in generated.iter().enumerate() {
        if finalized.items.get(index).copied() != Some(true) {
            continue;
        }
        tickets::insert_available(
            pool,
            &keypair.public_bytes(),
            seed,
            dim,
            network,
            &identity.inviter_ss58,
        )
        .await
        .context("inserting registered ticket")?;
        inserted += 1;
    }
    let failed = size - inserted;
    tracing::info!(
        dim = dim.as_str(),
        network = network.as_str(),
        block = %finalized.block_hash,
        submitted = size,
        registered = inserted,
        failed,
        "ticket batch finalized"
    );
    // A batch where every item failed did not make progress — throttle it
    // like a whole-batch failure instead of growing.
    Ok((
        inserted,
        settle_batch_size(batch_size, tuning.batch_max, inserted > 0),
    ))
}

/// Run the maintainer loop forever: each tick serves every configured pool
/// sequentially (one nonce lane — never two submissions in flight), guarded
/// by the advisory lock.
pub async fn run_loop(
    pool: PgPool,
    chain: ChainClient,
    identity: InviterIdentity,
    dims: Vec<Dim>,
    network: Network,
    tuning: PoolTuning,
) -> anyhow::Result<()> {
    let mut batch_sizes: Vec<(Dim, u32)> =
        dims.iter().map(|dim| (*dim, tuning.batch_max)).collect();
    loop {
        match acquire_tick_lock(&pool).await {
            Ok(Some(guard)) => {
                for (dim, batch_size) in &mut batch_sizes {
                    match tick_pool(
                        &pool,
                        &chain,
                        &identity,
                        *dim,
                        network,
                        &tuning,
                        *batch_size,
                    )
                    .await
                    {
                        Ok((_, next_size)) => *batch_size = next_size,
                        Err(err) => {
                            let throttled = settle_batch_size(*batch_size, tuning.batch_max, false);
                            tracing::warn!(
                                dim = dim.as_str(),
                                error = ?err,
                                batch_from = *batch_size,
                                batch_to = throttled,
                                "pool tick failed; keypairs discarded, batch throttled"
                            );
                            *batch_size = throttled;
                        }
                    }
                }
                drop(guard);
            }
            Ok(None) => {
                tracing::warn!("another maintainer instance holds the pool lock; skipping tick");
            }
            Err(err) => {
                tracing::warn!(error = ?err, "could not acquire pool lock; skipping tick");
            }
        }
        tokio::time::sleep(tuning.interval).await;
    }
}

/// A held advisory lock: released when the connection drops back to the pool.
struct TickLock {
    conn: sqlx::pool::PoolConnection<sqlx::Postgres>,
}

impl TickLock {
    async fn release(mut self) {
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(POOL_LOCK_KEY)
            .execute(&mut *self.conn)
            .await;
    }
}

/// Try to take the tick lock on a dedicated connection; `None` = held elsewhere.
async fn acquire_tick_lock(pool: &PgPool) -> anyhow::Result<Option<TickGuard>> {
    let mut conn = pool.acquire().await.context("acquiring lock connection")?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(POOL_LOCK_KEY)
        .fetch_one(&mut *conn)
        .await
        .context("taking advisory lock")?;
    if locked {
        Ok(Some(TickGuard {
            lock: Some(TickLock { conn }),
        }))
    } else {
        Ok(None)
    }
}

/// Drop-guard that releases the advisory lock without blocking `Drop`.
struct TickGuard {
    lock: Option<TickLock>,
}

impl Drop for TickGuard {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            tokio::spawn(lock.release());
        }
    }
}

/// Maintainer configuration, loaded from the environment (independent of the
/// api bin's [`crate::Config`] — the maintainer holds the inviter secret and
/// no JWT material; the api holds JWT material and no secret).
#[derive(Clone)]
pub struct MaintainerConfig {
    /// Postgres connection string (invite-tickets' own DB).
    pub database_url: String,
    /// People Chain RPC endpoint.
    pub people_rpc_url: String,
    /// Network partition this maintainer stocks.
    pub network: Network,
    /// Inviter whose quota funds tickets; stamped into rows.
    pub inviter_address: String,
    /// Secret of the hot inviter signing key (SURI or raw 64-byte hex).
    pub signer_secret: String,
    /// Optional cold inviter primary: when set, batches are wrapped in
    /// `Proxy.proxy(real = this account, force_proxy_type = Any, …)`.
    pub proxy_for: Option<AccountId32>,
    /// Which DIM pools to stock.
    pub dims: Vec<Dim>,
    /// Loop tunables.
    pub tuning: PoolTuning,
}

/// The inviter signing secret (and the DB password inside `database_url`) must
/// never reach logs, spans, or error output — a `{:?}` of the config anywhere
/// would otherwise leak them, so `Debug` is implemented by hand.
impl std::fmt::Debug for MaintainerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintainerConfig")
            .field("database_url", &"<redacted>")
            .field("people_rpc_url", &self.people_rpc_url)
            .field("network", &self.network)
            .field("inviter_address", &self.inviter_address)
            .field("signer_secret", &"<redacted>")
            .field("proxy_for", &self.proxy_for)
            .field("dims", &self.dims)
            .field("tuning", &self.tuning)
            .finish()
    }
}

impl MaintainerConfig {
    /// Read and validate maintainer configuration from the environment.
    ///
    /// Fails (rather than defaulting) for `DATABASE_URL`, `PEOPLE_RPC_URL`,
    /// `PEOPLE_NETWORK`, `INVITER_ADDRESS`, and `INVITER_SIGNER_SURI`.
    pub fn from_env() -> anyhow::Result<Self> {
        use std::str::FromStr as _;

        let database_url = required_env("DATABASE_URL")?;
        let people_rpc_url = required_env("PEOPLE_RPC_URL")?;
        let network_raw = required_env("PEOPLE_NETWORK")?;
        let network = Network::from_str(&network_raw)
            .map_err(|()| anyhow::anyhow!("PEOPLE_NETWORK must be westend2|paseo|polkadot"))?;

        let inviter_address = required_env("INVITER_ADDRESS")?;
        AccountId32::from_str(&inviter_address)
            .map_err(|e| anyhow::anyhow!("INVITER_ADDRESS is not valid SS58: {e}"))?;
        let signer_secret = required_env("INVITER_SIGNER_SURI")?;
        let proxy_for = match std::env::var("INVITER_PROXY_FOR") {
            Ok(raw) if !raw.trim().is_empty() => Some(
                AccountId32::from_str(raw.trim())
                    .map_err(|e| anyhow::anyhow!("INVITER_PROXY_FOR is not valid SS58: {e}"))?,
            ),
            _ => None,
        };

        let dims_raw = std::env::var("POOL_DIMS").unwrap_or_else(|_| "Game,ProofOfInk".to_string());
        let dims: Vec<Dim> = dims_raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                Dim::from_str(part)
                    .map_err(|()| anyhow::anyhow!("POOL_DIMS entries must be Game|ProofOfInk"))
            })
            .collect::<anyhow::Result<_>>()?;
        if dims.is_empty() {
            anyhow::bail!("POOL_DIMS must name at least one DIM");
        }

        let interval_secs = env_bounded_u64("POOL_INTERVAL_SECS", 6, 1, 86_400)?;
        let target = env_bounded_u64("POOL_TARGET_SIZE", 10_000, 1, 10_000_000)?;
        let batch_max = env_bounded_u64("POOL_BATCH_MAX", 100, 1, 1_000)?;
        let finalize_secs = env_bounded_u64("POOL_FINALIZE_SECS", 60, 1, 86_400)?;

        Ok(Self {
            database_url,
            people_rpc_url,
            network,
            inviter_address,
            signer_secret,
            proxy_for,
            dims,
            tuning: PoolTuning {
                interval: Duration::from_secs(interval_secs),
                target: i64::try_from(target).context("POOL_TARGET_SIZE is too large")?,
                batch_max: u32::try_from(batch_max).context("POOL_BATCH_MAX is too large")?,
                finalize_timeout: Duration::from_secs(finalize_secs),
            },
        })
    }
}

/// Connect and run the maintainer until fatal error (the process is expected
/// to be supervised and restarted).
pub async fn run(config: MaintainerConfig) -> anyhow::Result<()> {
    use std::str::FromStr as _;

    let signer = WriterSigner::from_secret(&config.signer_secret)?;
    let configured_inviter = AccountId32::from_str(&config.inviter_address)
        .context("parsing validated INVITER_ADDRESS")?;
    let signer_account = AccountId32(signer.public_bytes());
    let effective_inviter = config.proxy_for.as_ref().unwrap_or(&signer_account);
    if effective_inviter != &configured_inviter {
        anyhow::bail!(
            "effective signer account does not match INVITER_ADDRESS \
             (effective {effective_inviter}, configured {configured_inviter})"
        );
    }

    let pool = crate::db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    tracing::info!(
        people_rpc = %config.people_rpc_url,
        network = config.network.as_str(),
        inviter = %config.inviter_address,
        signer = %hex::encode(signer.public_bytes()),
        proxy = config.proxy_for.is_some(),
        dims = ?config.dims,
        target = config.tuning.target,
        "invite-tickets-pool starting"
    );

    let identity = InviterIdentity {
        signer,
        inviter: configured_inviter,
        inviter_ss58: config.inviter_address.clone(),
        proxy: config.proxy_for.is_some(),
    };
    run_loop(
        pool,
        chain,
        identity,
        config.dims.clone(),
        config.network,
        config.tuning.clone(),
    )
    .await
}

fn required_env(key: &'static str) -> anyhow::Result<String> {
    let raw =
        std::env::var(key).map_err(|_| anyhow::anyhow!("required env var {key} is not set"))?;
    let value = raw.trim();
    if value.is_empty() {
        anyhow::bail!("env var {key} must not be empty");
    }
    Ok(value.to_string())
}

/// Parse an env var as a bounded integer, defaulting when unset.
fn env_bounded_u64(key: &'static str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
    let raw = std::env::var(key).unwrap_or_else(|_| default.to_string());
    let value: u64 = raw
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("env var {key} is not a number: {e}"))?;
    if !(min..=max).contains(&value) {
        anyhow::bail!("env var {key} must be within {min}..={max}, got {value}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::settle_batch_size;

    #[test]
    fn grows_by_one_on_success_up_to_max() {
        assert_eq!(settle_batch_size(50, 100, true), 51);
        assert_eq!(settle_batch_size(100, 100, true), 100);
        assert_eq!(settle_batch_size(99, 100, true), 100);
    }

    #[test]
    fn halves_on_failure_with_floor_one() {
        assert_eq!(settle_batch_size(100, 100, false), 50);
        assert_eq!(settle_batch_size(3, 100, false), 1);
        assert_eq!(settle_batch_size(1, 100, false), 1);
    }
}
