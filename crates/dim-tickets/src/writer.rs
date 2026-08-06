//! `dim-tickets-writer` — the single-instance worker that registers pending
//! DIM tickets on People Chain.
//!
//! Single-instance by design: one inviter key = one nonce lane; never scale
//! it. A minimal Postgres lease guards deploy overlap (best-effort, not a
//! fence); the chain nonce plus the `retry_at` lease on `SUBMITTING` rows is
//! the real serializer.
//!
//! Each tick mirrors the legacy daemon exactly: fetch due rows (FIFO, batch
//! cap) → pure plan ([`crate::plan`]) → mark exhausted `FAILED`, recover
//! orphans, move the batch to `SUBMITTING` under one `retry_at` lease → submit
//! one `Utility.force_batch` → split per-item events into `REGISTERED` (with
//! `onchainData`) vs `PENDING` retries with incremented counts and backoff.
//! A whole-batch submission error leaves the rows `SUBMITTING`; their lease
//! expiry brings them back as orphans next tick (legacy behavior — such errors
//! do NOT increment `retry_count`).

use std::str::FromStr as _;
use std::time::Duration;

use anyhow::Context as _;
use chain_client::WriterSigner;
use sqlx::{PgPool, Row as _};
use subxt::utils::AccountId32;
use time::OffsetDateTime;

use crate::chain::ChainClient;
use crate::plan;
use crate::tickets::{self, Dim, Network};

const MAX_INTERVAL_SECS: u64 = 86_400;
const MAX_BATCH_SIZE: u64 = 1_000;
const MAX_RETRIES: u64 = 1_000;
const MAX_RETRY_DELAY_MS: u64 = 86_400_000;
const MAX_RETRY_EXPONENT: u64 = 63;

/// Writer configuration, loaded from the environment (independent of the api
/// bin's [`crate::Config`] — the writer needs the inviter secret and no JWT
/// material).
#[derive(Clone)]
pub struct WriterConfig {
    /// Postgres connection string (dim-tickets' own DB).
    pub database_url: String,
    /// People Chain RPC endpoint.
    pub people_rpc_url: String,
    /// Network partition this writer is allowed to process.
    pub network: Network,
    /// Inviter partition this writer is allowed to process.
    pub inviter_address: String,
    /// Secret of the hot inviter signing key (SURI or raw 64-byte hex).
    pub signer_secret: String,
    /// Optional cold inviter primary: when set, batches are wrapped in
    /// `Proxy.proxy(real = this account, force_proxy_type = Any, …)`.
    pub proxy_for: Option<AccountId32>,
    /// This writer instance's lease holder id.
    pub holder_id: String,
    /// Lease row name (all writers of one inviter share it).
    pub lease_name: String,
    /// Lease TTL / heartbeat expiry.
    pub lease_ttl: Duration,
    /// Idle poll interval between ticks (legacy: 6s).
    pub poll_interval: Duration,
    /// Max rows fetched per tick (legacy: 10).
    pub batch_size: i64,
    /// Retries before a ticket fails terminally (legacy: 5).
    pub max_retries: i32,
    /// Backoff base, ms (legacy: 1000).
    pub retry_base_ms: u64,
    /// Backoff cap, ms (legacy: 60000).
    pub retry_max_ms: u64,
    /// Backoff exponent cap (legacy: 10).
    pub retry_max_exponent: u32,
    /// Per-batch submit + finalization timeout (legacy: 120s).
    pub finalize_timeout: Duration,
}

/// The inviter signing secret (and the DB password inside `database_url`) must
/// never reach logs, spans, or error output — a `{:?}` of the config anywhere
/// would otherwise leak them, so `Debug` is implemented by hand.
impl std::fmt::Debug for WriterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterConfig")
            .field("database_url", &"<redacted>")
            .field("people_rpc_url", &self.people_rpc_url)
            .field("network", &self.network)
            .field("inviter_address", &self.inviter_address)
            .field("signer_secret", &"<redacted>")
            .field("proxy_for", &self.proxy_for)
            .field("holder_id", &self.holder_id)
            .field("lease_name", &self.lease_name)
            .field("lease_ttl", &self.lease_ttl)
            .field("poll_interval", &self.poll_interval)
            .field("batch_size", &self.batch_size)
            .field("max_retries", &self.max_retries)
            .field("retry_base_ms", &self.retry_base_ms)
            .field("retry_max_ms", &self.retry_max_ms)
            .field("retry_max_exponent", &self.retry_max_exponent)
            .field("finalize_timeout", &self.finalize_timeout)
            .finish()
    }
}

impl WriterConfig {
    /// Read and validate writer configuration from the environment.
    ///
    /// Fails (rather than defaulting) for `DATABASE_URL`, `PEOPLE_RPC_URL`,
    /// `PEOPLE_NETWORK`, `INVITER_ADDRESS`, and `INVITER_SIGNER_SURI`.
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = required_env("DATABASE_URL")?;
        let people_rpc_url = required_env("PEOPLE_RPC_URL")?;
        let network_raw = required_env("PEOPLE_NETWORK")?;
        let network = Network::from_str(&network_raw).map_err(|()| {
            anyhow::anyhow!(
                "PEOPLE_NETWORK is invalid: expected westend2|paseo|polkadot, got {network_raw}"
            )
        })?;
        let inviter_address = required_env("INVITER_ADDRESS")?;
        AccountId32::from_str(&inviter_address)
            .map_err(|e| anyhow::anyhow!("INVITER_ADDRESS is not valid SS58: {e}"))?;
        let signer_secret = required_env("INVITER_SIGNER_SURI")?;
        let proxy_for = match std::env::var("INVITER_PROXY_FOR") {
            Ok(v) if !v.trim().is_empty() => Some(
                AccountId32::from_str(v.trim())
                    .map_err(|e| anyhow::anyhow!("INVITER_PROXY_FOR is not valid SS58: {e}"))?,
            ),
            _ => None,
        };

        let lease_ttl_secs = env_bounded_u64("WRITER_LEASE_TTL_SECS", 30, 3, MAX_INTERVAL_SECS)?;
        let poll_secs = env_bounded_u64("WRITER_POLL_SECS", 6, 1, MAX_INTERVAL_SECS)?;
        let batch_size = env_bounded_u64("WRITER_BATCH_SIZE", 10, 1, MAX_BATCH_SIZE)?;
        let max_retries = env_bounded_u64("WRITER_MAX_RETRIES", 5, 1, MAX_RETRIES)?;
        let retry_base_ms = env_bounded_u64("WRITER_RETRY_BASE_MS", 1_000, 1, MAX_RETRY_DELAY_MS)?;
        let retry_max_ms = env_bounded_u64(
            "WRITER_RETRY_MAX_MS",
            60_000,
            retry_base_ms,
            MAX_RETRY_DELAY_MS,
        )?;
        let retry_max_exponent =
            env_bounded_u64("WRITER_RETRY_MAX_EXPONENT", 10, 1, MAX_RETRY_EXPONENT)?;
        let finalize_secs = env_bounded_u64("WRITER_FINALIZE_SECS", 120, 1, MAX_INTERVAL_SECS)?;

        Ok(Self {
            database_url,
            people_rpc_url,
            network,
            inviter_address,
            signer_secret,
            proxy_for,
            holder_id: std::env::var("WRITER_HOLDER_ID")
                .unwrap_or_else(|_| format!("dim-tickets-writer-{}", std::process::id())),
            lease_name: std::env::var("WRITER_LEASE_NAME")
                .unwrap_or_else(|_| "dim-tickets-writer".to_string()),
            lease_ttl: Duration::from_secs(lease_ttl_secs),
            poll_interval: Duration::from_secs(poll_secs),
            batch_size: i64::try_from(batch_size).context("WRITER_BATCH_SIZE is too large")?,
            max_retries: i32::try_from(max_retries).context("WRITER_MAX_RETRIES is too large")?,
            retry_base_ms,
            retry_max_ms,
            retry_max_exponent: u32::try_from(retry_max_exponent)
                .context("WRITER_RETRY_MAX_EXPONENT is too large")?,
            finalize_timeout: Duration::from_secs(finalize_secs),
        })
    }
}

/// Connect and run the writer until fatal error (the process is expected to be
/// supervised and restarted).
pub async fn run(config: WriterConfig) -> anyhow::Result<()> {
    let signer = WriterSigner::from_secret(&config.signer_secret)?;
    let configured_inviter = AccountId32::from_str(&config.inviter_address)
        .context("parsing validated INVITER_ADDRESS")?;
    let signer_account = AccountId32(signer.public_bytes());
    let effective_inviter = config.proxy_for.as_ref().unwrap_or(&signer_account);
    if effective_inviter != &configured_inviter {
        anyhow::bail!(
            "effective signer account does not match INVITER_ADDRESS (effective {effective_inviter}, configured {configured_inviter})"
        );
    }

    let pool = crate::db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    tracing::info!(
        people_rpc = %config.people_rpc_url,
        network = %config.network.as_str(),
        inviter = %config.inviter_address,
        signer = %hex::encode(signer.public_bytes()),
        proxy = config.proxy_for.is_some(),
        holder = %config.holder_id,
        "dim-tickets-writer connected"
    );
    http_common::metrics::spawn_readiness_probe(
        "dim-tickets-writer",
        (pool.clone(), chain.clone()),
        |(p, c)| crate::http::probe(p, c),
    );

    let writer = Writer {
        pool,
        chain,
        signer,
        config,
    };
    writer.run_forever().await
}

struct Writer {
    pool: PgPool,
    chain: ChainClient,
    signer: WriterSigner,
    config: WriterConfig,
}

/// Lease ownership proof carried through a tick.
struct Guard {
    epoch: i64,
}

impl Writer {
    async fn run_forever(&self) -> anyhow::Result<()> {
        loop {
            let guard = self.acquire_lease().await?;
            tracing::info!(epoch = guard.epoch, "acquired writer lease");
            if let Err(e) = self.active_loop(&guard).await {
                tracing::warn!(error = %e, "writer loop exited; re-acquiring lease");
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Claim (or renew) the single-writer lease, taking it only when free,
    /// expired, or already ours. Waits and retries while another writer holds it.
    async fn acquire_lease(&self) -> anyhow::Result<Guard> {
        let ttl = self.config.lease_ttl.as_secs() as i64;
        loop {
            if let Some(epoch) = try_acquire_lease(
                &self.pool,
                &self.config.lease_name,
                &self.config.holder_id,
                ttl,
            )
            .await?
            {
                return Ok(Guard { epoch });
            }
            tracing::info!("writer lease held by another instance; waiting");
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Renew the lease. Returns `false` if it is no longer ours.
    async fn heartbeat(&self, guard: &Guard) -> anyhow::Result<bool> {
        let ttl = self.config.lease_ttl.as_secs() as i64;
        Ok(heartbeat_lease(
            &self.pool,
            &self.config.lease_name,
            &self.config.holder_id,
            guard.epoch,
            ttl,
        )
        .await?)
    }

    async fn active_loop(&self, guard: &Guard) -> anyhow::Result<()> {
        loop {
            if !self.heartbeat(guard).await? {
                anyhow::bail!("lost writer lease");
            }
            if let Err(e) = self.tick(guard).await {
                // Row-level state is already durable; log and keep ticking.
                tracing::warn!(error = %e, "writer tick failed");
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// One legacy-shaped tick: fetch → plan ([`crate::plan`]) → execute. All
    /// decisions live in the pure plan functions; this is the I/O plumbing.
    async fn tick(&self, guard: &Guard) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let rows = tickets::fetch_due(
            &self.pool,
            now,
            self.config.batch_size,
            self.config.network,
            &self.config.inviter_address,
        )
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        tracing::info!(count = rows.len(), "processing batch");

        // Decode rows; malformed ones are logged and skipped (legacy behavior).
        let (due, skipped) = plan::decode_due(&rows);
        for (ticket, dim) in &skipped {
            tracing::warn!(ticket = %ticket, dim = %dim, "malformed ticket row skipped");
        }

        let delay = plan::retry_delay_ms(
            self.config.retry_base_ms,
            self.config.retry_max_ms,
            self.config.retry_max_exponent,
        );
        let plan = plan::plan_batch(&due, self.config.max_retries, delay);

        if !plan.exhausted.is_empty() {
            tracing::warn!(count = plan.exhausted.len(), "marking tickets exhausted");
            tickets::mark_failed(
                &self.pool,
                &plan.exhausted,
                self.config.network,
                &self.config.inviter_address,
            )
            .await?;
        }
        if !plan.recovered.is_empty() {
            tracing::info!(count = plan.recovered.len(), "recovering orphaned tickets");
            tickets::recover_orphaned(
                &self.pool,
                &plan.recovered,
                self.config.network,
                &self.config.inviter_address,
            )
            .await?;
        }
        if plan.batch.is_empty() {
            return Ok(());
        }

        // Parse the batch addresses; a row whose key is not valid SS58 cannot
        // ever submit — fail it terminally rather than looping on it.
        let (batch, unparseable) = plan::parse_batch(&plan.batch);
        for (ticket, error) in &unparseable {
            tracing::warn!(ticket = %ticket, error = %error, "unparseable ticket address; failing terminally");
            tickets::mark_failed(
                &self.pool,
                std::slice::from_ref(ticket),
                self.config.network,
                &self.config.inviter_address,
            )
            .await?;
        }
        if batch.is_empty() {
            return Ok(());
        }

        let ticket_ids: Vec<String> = batch.iter().map(|(t, _, _)| t.clone()).collect();
        let retry_counts = plan::batch_retry_counts(&ticket_ids, &due);
        let lease_until = now + time::Duration::milliseconds(plan.lease_delay_ms as i64);
        tickets::mark_submitting(
            &self.pool,
            &ticket_ids,
            lease_until,
            self.config.network,
            &self.config.inviter_address,
        )
        .await?;

        let calls: Vec<(AccountId32, Dim)> = batch.iter().map(|(_, a, d)| (*a, *d)).collect();
        let submit = tokio::time::timeout(
            self.config.finalize_timeout,
            self.chain
                .submit_invite_batch(&calls, &self.signer, self.config.proxy_for.as_ref()),
        );
        // Renew the lease while awaiting finalization — it can outlast the TTL.
        let finalized = {
            tokio::pin!(submit);
            let mut renew = tokio::time::interval(self.config.lease_ttl / 3);
            renew.tick().await; // consume the immediate first tick
            loop {
                tokio::select! {
                    result = &mut submit => break result,
                    _ = renew.tick() => {
                        if !self.heartbeat(guard).await? {
                            anyhow::bail!("lost writer lease during submit");
                        }
                    }
                }
            }
        };

        let finalized = match finalized {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                // Rows stay SUBMITTING; the retry_at lease brings them back as
                // orphans next tick (no retry_count increment — legacy).
                tracing::warn!(error = %e, "batch submission failed; rows recover via lease");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "batch submission/finalization timed out; rows recover via lease"
                );
                return Ok(());
            }
        };

        let split = plan::split_batch_items(&finalized.items, batch.len());
        for warning in &split.warnings {
            tracing::warn!(warning = %warning, "force_batch event mismatch");
        }

        let (registered, retries) = plan::apply_split(&split, &ticket_ids, &retry_counts, delay);
        let onchain_data = serde_json::json!({
            "blockHash": finalized.block_hash,
            "blockNumber": finalized.block_number,
        });
        tickets::mark_registered(
            &self.pool,
            &registered,
            &onchain_data,
            self.config.network,
            &self.config.inviter_address,
        )
        .await?;

        let retry_now = OffsetDateTime::now_utc();
        for retry in &retries {
            let retry_at = retry_now + time::Duration::milliseconds(retry.delay_ms as i64);
            tickets::schedule_retry(
                &self.pool,
                &retry.ticket,
                retry.retry_count,
                retry_at,
                self.config.network,
                &self.config.inviter_address,
            )
            .await?;
        }

        tracing::info!(
            registered = registered.len(),
            failed = split.failed.len(),
            block = finalized.block_number,
            "batch submission completed"
        );
        Ok(())
    }
}

/// One attempt to claim (or renew) the single-writer lease: takes it only
/// when the row is free, expired, or already held by `holder_id`; every take
/// bumps `lease_epoch`. `None` means another live writer holds it.
///
/// Test-only seam for the live-Postgres suite (`tests/tickets_live_pg.rs`);
/// not part of the crate's API.
#[doc(hidden)]
pub async fn try_acquire_lease(
    pool: &PgPool,
    name: &str,
    holder_id: &str,
    ttl_secs: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let row = sqlx::query(
        "INSERT INTO writer_lease (name, holder_id, lease_epoch, expires_at, heartbeat_at) \
         VALUES ($1, $2, 1, now() + ($3 * interval '1 second'), now()) \
         ON CONFLICT (name) DO UPDATE \
           SET holder_id = EXCLUDED.holder_id, \
               lease_epoch = writer_lease.lease_epoch + 1, \
               expires_at = now() + ($3 * interval '1 second'), \
               heartbeat_at = now() \
           WHERE writer_lease.expires_at < now() OR writer_lease.holder_id = $2 \
         RETURNING lease_epoch",
    )
    .bind(name)
    .bind(holder_id)
    .bind(ttl_secs)
    .fetch_optional(pool)
    .await?;
    row.map(|row| row.try_get("lease_epoch")).transpose()
}

/// Renew the lease. `false` means it is no longer ours (a takeover changed
/// the holder or bumped the epoch).
///
/// Test-only seam for the live-Postgres suite (`tests/tickets_live_pg.rs`);
/// not part of the crate's API.
#[doc(hidden)]
pub async fn heartbeat_lease(
    pool: &PgPool,
    name: &str,
    holder_id: &str,
    epoch: i64,
    ttl_secs: i64,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE writer_lease \
         SET expires_at = now() + ($4 * interval '1 second'), heartbeat_at = now() \
         WHERE name = $1 AND holder_id = $2 AND lease_epoch = $3",
    )
    .bind(name)
    .bind(holder_id)
    .bind(epoch)
    .bind(ttl_secs)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

fn required_env(key: &str) -> anyhow::Result<String> {
    let raw = std::env::var(key).with_context(|| format!("{key} is required"))?;
    let value = raw.trim();
    if value.is_empty() {
        anyhow::bail!("{key} must not be empty");
    }
    Ok(value.to_string())
}

fn env_bounded_u64(key: &str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
    let raw = match std::env::var(key) {
        Ok(raw) => Some(raw),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{key} is not valid Unicode"),
    };
    parse_bounded_u64(key, raw.as_deref(), default, min, max)
}

fn parse_bounded_u64(
    key: &str,
    raw: Option<&str>,
    default: u64,
    min: u64,
    max: u64,
) -> anyhow::Result<u64> {
    let value = match raw {
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{key} must be an integer"))?,
        None => default,
    };
    if !(min..=max).contains(&value) {
        anyhow::bail!("{key} must be between {min} and {max}, got {value}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_writer_values_reject_malformed_zero_and_overflow() {
        assert!(parse_bounded_u64("TEST", Some("nope"), 10, 1, 100).is_err());
        assert!(parse_bounded_u64("TEST", Some("0"), 10, 1, 100).is_err());
        assert!(parse_bounded_u64("TEST", Some("101"), 10, 1, 100).is_err());
        assert!(parse_bounded_u64("TEST", Some("18446744073709551616"), 10, 1, u64::MAX,).is_err());
    }

    #[test]
    fn bounded_writer_values_use_defaults_only_when_unset() {
        assert_eq!(parse_bounded_u64("TEST", None, 10, 1, 100).unwrap(), 10);
        assert_eq!(
            parse_bounded_u64("TEST", Some(" 42 "), 10, 1, 100).unwrap(),
            42
        );
    }

    #[test]
    fn debug_output_redacts_signer_secret_and_db_password() {
        let config = WriterConfig {
            database_url: "postgres://dim:s3cret-db-pw@localhost/dim".to_string(),
            people_rpc_url: "wss://rpc.example".to_string(),
            network: Network::Paseo,
            inviter_address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".to_string(),
            signer_secret: "s3cret-inviter-suri".to_string(),
            proxy_for: None,
            holder_id: "test".to_string(),
            lease_name: "test".to_string(),
            lease_ttl: Duration::from_secs(30),
            poll_interval: Duration::from_secs(6),
            batch_size: 10,
            max_retries: 5,
            retry_base_ms: 1_000,
            retry_max_ms: 60_000,
            retry_max_exponent: 10,
            finalize_timeout: Duration::from_secs(120),
        };
        let dump = format!("{config:?}");
        assert!(!dump.contains("s3cret-inviter-suri"));
        assert!(!dump.contains("s3cret-db-pw"));
        assert!(dump.contains("<redacted>"));
        assert!(dump.contains("wss://rpc.example")); // non-secrets stay visible
    }

    /// Every env var `from_env` reads, cleared around each case so neither a
    /// previous case nor the developer's ambient shell leaks into the next.
    const FROM_ENV_VARS: &[&str] = &[
        "DATABASE_URL",
        "PEOPLE_RPC_URL",
        "PEOPLE_NETWORK",
        "INVITER_ADDRESS",
        "INVITER_SIGNER_SURI",
        "INVITER_PROXY_FOR",
        "WRITER_HOLDER_ID",
        "WRITER_LEASE_NAME",
        "WRITER_LEASE_TTL_SECS",
        "WRITER_POLL_SECS",
        "WRITER_BATCH_SIZE",
        "WRITER_MAX_RETRIES",
        "WRITER_RETRY_BASE_MS",
        "WRITER_RETRY_MAX_MS",
        "WRITER_RETRY_MAX_EXPONENT",
        "WRITER_FINALIZE_SECS",
    ];

    /// The five vars `from_env` fails on rather than defaults.
    const REQUIRED_ENV: &[(&str, &str)] = &[
        ("DATABASE_URL", "postgres://dim:pw@localhost/dim"),
        ("PEOPLE_RPC_URL", "wss://rpc.example"),
        ("PEOPLE_NETWORK", "paseo"),
        (
            "INVITER_ADDRESS",
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY", // Alice
        ),
        ("INVITER_SIGNER_SURI", "//Inviter"),
    ];

    /// Bob's well-known dev account: the SS58 form and the sr25519 public key
    /// bytes it encodes, so the proxy parse is asserted against an independent
    /// constant rather than round-tripped through the same parser.
    const BOB_SS58: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
    const BOB_HEX: &str = "8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";

    /// Clear the writer's vars, set `vars`, run `from_env`, then clear again —
    /// success and error paths alike leave the process environment clean.
    fn from_env_with(vars: &[(&str, &str)]) -> anyhow::Result<WriterConfig> {
        for key in FROM_ENV_VARS {
            std::env::remove_var(key);
        }
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        let result = WriterConfig::from_env();
        for key in FROM_ENV_VARS {
            std::env::remove_var(key);
        }
        result
    }

    /// Env vars are process-global and cargo runs `#[test]`s on parallel
    /// threads, so ALL `from_env` cases run sequentially inside this one test
    /// — never split them into separate `#[test]` fns. (No other test in this
    /// binary reads or writes these vars; the `parse_bounded_u64` tests above
    /// take their raw value as an argument.)
    #[test]
    fn from_env_reads_and_validates_the_environment() {
        // Fully populated: every var lands in its field, and required values
        // are trimmed of accidental padding.
        let config = from_env_with(&[
            ("DATABASE_URL", "  postgres://dim:pw@localhost/dim  "),
            ("PEOPLE_RPC_URL", "wss://rpc.example"),
            ("PEOPLE_NETWORK", "westend2"),
            (
                "INVITER_ADDRESS",
                "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY",
            ),
            ("INVITER_SIGNER_SURI", "//Inviter"),
            ("INVITER_PROXY_FOR", BOB_SS58),
            ("WRITER_HOLDER_ID", "writer-test-1"),
            ("WRITER_LEASE_NAME", "custom-lease"),
            ("WRITER_LEASE_TTL_SECS", "45"),
            ("WRITER_POLL_SECS", "3"),
            ("WRITER_BATCH_SIZE", "50"),
            ("WRITER_MAX_RETRIES", "7"),
            ("WRITER_RETRY_BASE_MS", "500"),
            ("WRITER_RETRY_MAX_MS", "30000"),
            ("WRITER_RETRY_MAX_EXPONENT", "8"),
            ("WRITER_FINALIZE_SECS", "90"),
        ])
        .unwrap();
        assert_eq!(config.database_url, "postgres://dim:pw@localhost/dim");
        assert_eq!(config.people_rpc_url, "wss://rpc.example");
        assert_eq!(config.network, Network::Westend2);
        assert_eq!(
            config.inviter_address,
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        );
        assert_eq!(config.signer_secret, "//Inviter");
        let bob: [u8; 32] = hex::decode(BOB_HEX).unwrap().try_into().unwrap();
        assert_eq!(config.proxy_for, Some(AccountId32(bob)));
        assert_eq!(config.holder_id, "writer-test-1");
        assert_eq!(config.lease_name, "custom-lease");
        assert_eq!(config.lease_ttl, Duration::from_secs(45));
        assert_eq!(config.poll_interval, Duration::from_secs(3));
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.max_retries, 7);
        assert_eq!(config.retry_base_ms, 500);
        assert_eq!(config.retry_max_ms, 30_000);
        assert_eq!(config.retry_max_exponent, 8);
        assert_eq!(config.finalize_timeout, Duration::from_secs(90));

        // Required-only: every optional var falls back to its legacy default,
        // and a whitespace-only proxy counts as unset.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("INVITER_PROXY_FOR", "   "));
        let config = from_env_with(&vars).unwrap();
        assert_eq!(config.network, Network::Paseo);
        assert_eq!(config.proxy_for, None);
        assert_eq!(
            config.holder_id,
            format!("dim-tickets-writer-{}", std::process::id())
        );
        assert_eq!(config.lease_name, "dim-tickets-writer");
        assert_eq!(config.lease_ttl, Duration::from_secs(30));
        assert_eq!(config.poll_interval, Duration::from_secs(6));
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.retry_base_ms, 1_000);
        assert_eq!(config.retry_max_ms, 60_000);
        assert_eq!(config.retry_max_exponent, 10);
        assert_eq!(config.finalize_timeout, Duration::from_secs(120));

        // Each missing required var fails naming exactly that var.
        for missing in [
            "DATABASE_URL",
            "PEOPLE_RPC_URL",
            "PEOPLE_NETWORK",
            "INVITER_ADDRESS",
            "INVITER_SIGNER_SURI",
        ] {
            let vars: Vec<_> = REQUIRED_ENV
                .iter()
                .copied()
                .filter(|(key, _)| *key != missing)
                .collect();
            let err = from_env_with(&vars).unwrap_err();
            assert!(
                err.to_string().contains(&format!("{missing} is required")),
                "{missing}: {err}"
            );
        }

        // A required var set to whitespace is rejected, not treated as a value.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("INVITER_SIGNER_SURI", "   ")); // later set_var wins
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("INVITER_SIGNER_SURI must not be empty"),
            "{err}"
        );

        // The network partition only accepts the frozen literals.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("PEOPLE_NETWORK", "kusama"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("PEOPLE_NETWORK is invalid: expected westend2|paseo|polkadot"),
            "{err}"
        );

        // Inviter and proxy addresses must be valid SS58, naming the var.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("INVITER_ADDRESS", "not-an-address"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("INVITER_ADDRESS is not valid SS58"),
            "{err}"
        );
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("INVITER_PROXY_FOR", "not-an-address"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("INVITER_PROXY_FOR is not valid SS58"),
            "{err}"
        );

        // Bounded knobs reject out-of-range and malformed values, naming the
        // var and its bounds.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("WRITER_LEASE_TTL_SECS", "2")); // below the min of 3
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("WRITER_LEASE_TTL_SECS must be between 3 and 86400, got 2"),
            "{err}"
        );
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("WRITER_BATCH_SIZE", "1001")); // above the max of 1000
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("WRITER_BATCH_SIZE must be between 1 and 1000, got 1001"),
            "{err}"
        );
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("WRITER_POLL_SECS", "soon"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("WRITER_POLL_SECS must be an integer"),
            "{err}"
        );

        // The backoff cap's lower bound is the configured base: a max below
        // the base can never be reached and is rejected as inconsistent.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("WRITER_RETRY_BASE_MS", "5000"));
        vars.push(("WRITER_RETRY_MAX_MS", "4999"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("WRITER_RETRY_MAX_MS must be between 5000 and 86400000, got 4999"),
            "{err}"
        );
    }
}
