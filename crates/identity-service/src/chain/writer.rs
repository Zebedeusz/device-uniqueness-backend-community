//! `identity-chain-writer` — the single-instance worker that drains the reservation
//! outbox onto People Chain.
//!
//! Separate from the N-replica `identity-api`: exactly one live signer per
//! account means one nonce lane. A minimal Postgres lease guards deploy overlap
//! (best-effort, not a fence); the chain nonce + reconciliation is the real
//! serializer. Each registration is submitted directly as `PeopleLite.attest`,
//! optionally wrapped in `Proxy.proxy` of a cold attester authority. `SUBMITTING`
//! is recorded before awaiting inclusion; an unknown
//! outcome is reconciled against `UsernameOwnerOf`, never blindly resubmitted.
//!
//! Follows an established chain-writer pattern (subxt 0.50, proxy signing,
//! cached nonce reset on error).

use std::str::FromStr as _;
use std::time::Duration;

use anyhow::Context as _;
use chain_types::{PeopleConfig, PeopleExtrinsicParamsBuilder};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::PgPool;
use subxt::dynamic::{self, Value};
use subxt::tx::{DynamicPayload, TransactionStatus};
use subxt::utils::AccountId32;
use time::OffsetDateTime;

use chain_client::WriterSigner;

use super::client::ChainClient;
use super::lease;
use super::outbox::{self, Guard, Reservation};

/// Chain-writer configuration, loaded from the environment.
#[derive(Debug, Clone)]
pub struct WriterConfig {
    /// Postgres connection string (shared schema with identity-api). May embed a
    /// password, so it is a secret.
    pub database_url: SecretString,
    /// People Chain RPC endpoint.
    pub people_rpc_url: String,
    /// SURI of the hot signing key (the proxy key, or the primary in dev).
    pub signer_suri: SecretString,
    /// Optional cold attester authority: when set, calls are wrapped in
    /// `Proxy.proxy(real = this account, …)` and the SURI signs as its proxy.
    pub proxy_for: Option<[u8; 32]>,
    /// This writer instance's lease holder id.
    pub holder_id: String,
    /// Lease row name (all writers of one account share it).
    pub lease_name: String,
    /// Lease TTL / heartbeat expiry.
    pub lease_ttl: Duration,
    /// Idle poll interval between outbox scans.
    pub poll_interval: Duration,
    /// Max rows claimed per scan.
    pub batch_size: i64,
    /// Per-submit finalization timeout.
    pub finalize_timeout: Duration,
    /// Max submit attempts before a row is failed terminally.
    pub max_attempts: i32,
    /// Whether the registration queue is enabled (`QUEUE_ENABLED` — must
    /// match identity-api's value). On: `QUEUED` rows are never drained here;
    /// a dead advancer only raises the stranded-queue warning, keeping the
    /// free lane's throttle intact. Off: this writer is the janitor that
    /// drains leftover `QUEUED` rows so retiring the queue strands nothing.
    pub queue_enabled: bool,
    /// Janitor grace (queue disabled only): how long the advancer's lease may
    /// be expired before leftover `QUEUED` rows are drained — the window that
    /// lets a live advancer finish a fair drain during the retire sequence.
    /// Doubles as the warning cadence while the queue is enabled.
    pub queue_fallback_after: Duration,
    /// Cadence of the payment watch pass (deposit detection + expiry over
    /// `payment_requests`). Read-only on chain; a no-op while the payment
    /// lane has never quoted anything.
    pub payment_poll_interval: Duration,
    /// Cadence of the attester-resources pass: read the attestation allowance
    /// and the account balances that registration silently dies without.
    pub resource_poll_interval: Duration,
    /// WARN below this many remaining attestations.
    pub allowance_floor: u32,
    /// WARN below this signer free balance, in planck (transaction fees come
    /// from the signer, not the proxied primary).
    pub signer_balance_floor_planck: u128,
}

impl WriterConfig {
    /// Read and validate writer configuration from the environment.
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
        let people_rpc_url = std::env::var("PEOPLE_RPC_URL")
            .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
        let signer_suri = std::env::var("CHAIN_WRITER_SIGNER_SURI")
            .context("CHAIN_WRITER_SIGNER_SURI is required")?;
        let proxy_for = match std::env::var("CHAIN_WRITER_PROXY_FOR") {
            Ok(v) if !v.trim().is_empty() => Some(parse_account(v.trim())?),
            _ => None,
        };
        // The default must be unique per boot: in a container the PID is
        // always 1, and identical holder ids let overlapping instances steal
        // each other's live lease through the "already ours" arm.
        let holder_id = match std::env::var("CHAIN_WRITER_HOLDER_ID") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => format!(
                "writer-{}-{:08x}",
                std::process::id(),
                rand::random::<u32>()
            ),
        };

        Ok(Self {
            database_url: SecretString::from(database_url),
            people_rpc_url,
            signer_suri: SecretString::from(signer_suri),
            proxy_for,
            holder_id,
            lease_name: std::env::var("CHAIN_WRITER_LEASE_NAME")
                .unwrap_or_else(|_| "people-chain-writer".to_string()),
            lease_ttl: Duration::from_secs(env_u64("CHAIN_WRITER_LEASE_TTL_SECS", 30)),
            poll_interval: Duration::from_secs(env_u64("CHAIN_WRITER_POLL_SECS", 2)),
            batch_size: env_u64("CHAIN_WRITER_BATCH_SIZE", 25) as i64,
            finalize_timeout: Duration::from_secs(env_u64("CHAIN_WRITER_FINALIZE_SECS", 120)),
            max_attempts: env_u64("CHAIN_WRITER_MAX_ATTEMPTS", 8) as i32,
            queue_enabled: crate::config::env_bool("QUEUE_ENABLED", false)?,
            queue_fallback_after: Duration::from_secs(crate::queue::env_u64_strict(
                "QUEUE_FALLBACK_AFTER_SECS",
                60,
            )?),
            payment_poll_interval: Duration::from_secs(crate::queue::env_u64_strict(
                "PAYMENT_POLL_INTERVAL_SECS",
                30,
            )?),
            resource_poll_interval: Duration::from_secs(crate::queue::env_u64_strict(
                "ATTESTER_RESOURCE_POLL_SECS",
                60,
            )?),
            allowance_floor: u32::try_from(crate::queue::env_u64_strict(
                "ATTESTER_ALLOWANCE_FLOOR",
                100,
            )?)
            .context("ATTESTER_ALLOWANCE_FLOOR must fit a u32")?,
            signer_balance_floor_planck: u128::from(crate::queue::env_u64_strict(
                "ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK",
                10_000_000_000,
            )?),
        })
    }
}

/// Connect and run the writer until fatal error (the process is expected to be
/// supervised and restarted).
pub async fn run(config: WriterConfig) -> anyhow::Result<()> {
    tracing::info!(
        people_rpc = %config.people_rpc_url,
        proxy = config.proxy_for.is_some(),
        holder = %config.holder_id,
        "starting identity-chain-writer"
    );
    let pool = crate::db::connect(config.database_url.expose_secret()).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    let signer = WriterSigner::from_secret(config.signer_suri.expose_secret())?;
    let signer_account = AccountId32(signer.public_bytes());
    tracing::info!(signer = %hex_account(&signer_account.0), "identity-chain-writer connected");
    // No HTTP surface here, so this is the only readiness signal it has.
    http_common::metrics::spawn_readiness_probe(
        "identity-chain-writer",
        (pool.clone(), chain.clone()),
        |(p, c)| crate::http::health::probe(p, c),
    );

    let mut writer = Writer {
        pool,
        chain,
        signer,
        signer_account,
        config,
        next_nonce: None,
    };
    writer.run_forever().await
}

struct Writer {
    pool: PgPool,
    chain: ChainClient,
    signer: WriterSigner,
    signer_account: AccountId32,
    config: WriterConfig,
    /// Locally-tracked nonce; reset to a chain re-fetch on any error.
    next_nonce: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitFailureAction {
    Assign,
    Retry,
    Fail,
}

fn classify_submit_failure(
    reason: &str,
    observed_owner: Option<[u8; 32]>,
    candidate: [u8; 32],
    completed_attempts: i32,
    max_attempts: i32,
) -> SubmitFailureAction {
    if observed_owner == Some(candidate) || reason.contains("AlreadyRegistered") {
        SubmitFailureAction::Assign
    } else if completed_attempts >= max_attempts {
        SubmitFailureAction::Fail
    } else {
        SubmitFailureAction::Retry
    }
}

impl Writer {
    async fn run_forever(&mut self) -> anyhow::Result<()> {
        loop {
            let guard = self.acquire_lease().await?;
            tracing::info!(epoch = guard.epoch, "acquired writer lease");
            self.next_nonce = None;
            if let Err(e) = self.reconcile_submitting(&guard).await {
                tracing::warn!(error = %e, "startup reconcile failed");
            }
            if let Err(e) = self.active_loop(&guard).await {
                tracing::warn!(error = %e, "writer loop exited; re-acquiring lease");
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Claim (or renew) the single-writer lease, taking it only when free,
    /// expired, or already ours. Waits and retries while another writer holds it.
    async fn acquire_lease(&self) -> anyhow::Result<Guard> {
        loop {
            let epoch = lease::try_acquire(
                &self.pool,
                &self.config.lease_name,
                &self.config.holder_id,
                self.config.lease_ttl,
            )
            .await?;
            if let Some(epoch) = epoch {
                return Ok(Guard {
                    lease_name: self.config.lease_name.clone(),
                    holder_id: self.config.holder_id.clone(),
                    epoch,
                });
            }
            tracing::info!("writer lease held by another instance; waiting");
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Renew the lease. Returns `false` if it is no longer ours.
    async fn heartbeat(&self, guard: &Guard) -> anyhow::Result<bool> {
        Ok(lease::renew(
            &self.pool,
            &guard.lease_name,
            &guard.holder_id,
            guard.epoch,
            self.config.lease_ttl,
        )
        .await?)
    }

    async fn active_loop(&mut self, guard: &Guard) -> anyhow::Result<()> {
        let mut last_payment_pass: Option<std::time::Instant> = None;
        let mut last_stranded_check: Option<std::time::Instant> = None;
        let mut last_resource_pass: Option<std::time::Instant> = None;
        loop {
            if !self.heartbeat(guard).await? {
                anyhow::bail!("lost writer lease");
            }
            // Attester resources: the two values that stop registration without
            // any error of their own — a spent allowance and an unfunded signer.
            if last_resource_pass.is_none_or(|t| t.elapsed() >= self.config.resource_poll_interval)
            {
                last_resource_pass = Some(std::time::Instant::now());
                if let Err(e) = self.log_attester_resources().await {
                    tracing::warn!(error = %e, "attester resources read failed");
                }
                if let Err(e) = record_outbox_gauges(&self.pool).await {
                    tracing::warn!(error = %e, "outbox gauge pass failed");
                }
            }
            if self.config.queue_enabled {
                // The queue is the free lane's throttle: never drain QUEUED
                // rows while it is enabled. A dead advancer with claims
                // waiting is an operator page, raised here (the always-on
                // process already scanning the outbox) at the grace cadence.
                if last_stranded_check
                    .is_none_or(|t| t.elapsed() >= self.config.queue_fallback_after)
                {
                    last_stranded_check = Some(std::time::Instant::now());
                    match crate::queue::stranded_queued(&self.pool).await {
                        Ok(0) => {}
                        Ok(stranded) => tracing::warn!(
                            stranded,
                            "queue advancer is down with claims queued; holding the throttle \
                             (queue enabled — not draining). Restart registration-queue, or \
                             retire the queue by setting QUEUE_ENABLED=false everywhere."
                        ),
                        Err(e) => tracing::warn!(error = %e, "stranded-queue check failed"),
                    }
                }
            } else {
                // Queue disabled: janitor duty — drain QUEUED rows left from
                // when it was on, once the advancer lease has been dead past
                // the grace window (a live advancer drains fairly first).
                match crate::queue::fallback_drain(&self.pool, self.config.queue_fallback_after)
                    .await
                {
                    Ok(0) => {}
                    Ok(drained) => tracing::warn!(
                        drained,
                        "queue disabled with advancer gone; promoted leftover queued claims"
                    ),
                    Err(e) => tracing::warn!(error = %e, "queue janitor drain failed"),
                }
            }
            // Payment watch pass (eligibility slice): expire stale quotes and
            // hand confirmed deposits to the outbox. Read-only on chain — the
            // one-signer nonce lane is untouched. A failed pass only delays
            // detection; PENDING rows are re-checked next time.
            if last_payment_pass.is_none_or(|t| t.elapsed() >= self.config.payment_poll_interval) {
                last_payment_pass = Some(std::time::Instant::now());
                match crate::payment::watch_pass(&self.pool, &self.chain).await {
                    Ok(stats) if stats.acted() => tracing::info!(
                        expired = stats.expired,
                        confirmed = stats.confirmed,
                        conflicted = stats.conflicted,
                        still_pending = stats.still_pending,
                        "payment watch pass"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "payment watch pass failed"),
                }
            }
            let due = outbox::claim_due(&self.pool, self.config.batch_size).await?;
            if due.is_empty() {
                tokio::time::sleep(self.config.poll_interval).await;
                continue;
            }
            for r in &due {
                // Renew before each row: a batch drains sequentially and each
                // submit awaits finalization, so a batch can outlast the lease TTL.
                if !self.heartbeat(guard).await? {
                    anyhow::bail!("lost writer lease");
                }
                self.process(guard, r).await?;
            }
        }
    }

    /// Process one reservation. Row-level chain failures are recorded on the row
    /// (retry/fail); only a lost lease returns `Err` to break the loop.
    async fn process(&mut self, guard: &Guard, r: &Reservation) -> anyhow::Result<()> {
        let Some(candidate) = parse_account(&r.candidate_account_id).ok() else {
            return self.fail(guard, r, "invalid candidate SS58").await;
        };

        // Idempotency pre-check: already owned means nothing to submit.
        match self.chain.username_owner(&r.full_username).await {
            Ok(Some(owner)) if owner == candidate => return self.assign(guard, r).await,
            Ok(Some(_)) => {
                return self
                    .fail(guard, r, "username owned by another account")
                    .await
            }
            Ok(None) => {}
            Err(e) => {
                return self
                    .retry(guard, r, &format!("owner read failed: {e}"))
                    .await
            }
        }

        let payload = build_registration_tx(r, &candidate, self.config.proxy_for.as_ref());
        let nonce = match self.nonce().await {
            Ok(n) => n,
            Err(e) => return self.retry(guard, r, &format!("nonce fetch: {e}")).await,
        };

        match self.submit(guard, r, &payload, nonce).await {
            Ok(()) => {
                self.next_nonce = Some(nonce + 1);
                self.assign(guard, r).await
            }
            Err(e) => {
                // Reset the cached nonce; re-fetch on the next attempt.
                self.next_nonce = None;
                let reason = e.to_string();

                // Reconcile: the extrinsic may have landed despite the error.
                let observed_owner = self
                    .chain
                    .username_owner(&r.full_username)
                    .await
                    .ok()
                    .flatten();
                match classify_submit_failure(
                    &reason,
                    observed_owner,
                    candidate,
                    r.attempt + 1,
                    self.config.max_attempts,
                ) {
                    SubmitFailureAction::Assign => self.assign(guard, r).await,
                    SubmitFailureAction::Retry => self.retry(guard, r, &reason).await,
                    SubmitFailureAction::Fail => {
                        self.fail(guard, r, &format!("max attempts reached: {reason}"))
                            .await
                    }
                }
            }
        }
    }

    /// Sign + submit, recording `SUBMITTING` (tx hash + nonce) before awaiting
    /// inclusion so a crash mid-flight reconciles instead of resubmitting.
    async fn submit(
        &self,
        guard: &Guard,
        r: &Reservation,
        payload: &DynamicPayload<Vec<Value>>,
        nonce: u64,
    ) -> anyhow::Result<()> {
        let params = PeopleExtrinsicParamsBuilder::<PeopleConfig>::new()
            .nonce(nonce)
            .build();
        let mut tx_client = self.chain.online().tx().await?;
        let signed = tx_client
            .create_signed(payload, &self.signer, params)
            .await?;
        let tx_hash = format!("{:?}", signed.hash());

        if !outbox::mark_submitting(
            &self.pool,
            guard,
            r.id,
            &tx_hash,
            nonce as i64,
            r.attempt + 1,
        )
        .await?
        {
            anyhow::bail!("lease lost before submit");
        }
        tracing::info!(id = r.id, username = %r.full_username, nonce, tx = %tx_hash, "submitting registration");

        let progress = signed.submit_and_watch().await?;
        let wait = async {
            let mut progress = progress;
            loop {
                match progress.next().await {
                    Some(status) => match status? {
                        TransactionStatus::InFinalizedBlock(in_block) => {
                            in_block.wait_for_success().await?;
                            return anyhow::Ok(());
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
        // Renew the lease while awaiting finalization: the finalize timeout can
        // exceed the lease TTL, so without this the lease could lapse mid-submit
        // and let a second writer reconcile an in-flight row. Losing the lease
        // stops the wait and hands the row to the reconcile path.
        let watched = async move {
            tokio::pin!(wait);
            let mut renew = tokio::time::interval(self.config.lease_ttl / 3);
            renew.tick().await; // consume the immediate first tick
            loop {
                tokio::select! {
                    result = &mut wait => return result,
                    _ = renew.tick() => {
                        if !self.heartbeat(guard).await? {
                            anyhow::bail!("lost writer lease during submit");
                        }
                    }
                }
            }
        };
        tokio::time::timeout(self.config.finalize_timeout, watched)
            .await
            .map_err(|_| anyhow::anyhow!("finalization timed out"))??;
        Ok(())
    }

    /// Drain `SUBMITTING` rows left by a previous writer: reconcile each against
    /// chain state (owned -> assigned; otherwise re-queue).
    async fn reconcile_submitting(&self, guard: &Guard) -> anyhow::Result<()> {
        for r in outbox::submitting(&self.pool).await? {
            let Some(candidate) = parse_account(&r.candidate_account_id).ok() else {
                self.fail(guard, &r, "invalid candidate SS58").await?;
                continue;
            };
            match self.chain.username_owner(&r.full_username).await? {
                Some(owner) if owner == candidate => self.assign(guard, &r).await?,
                _ => {
                    self.retry(guard, &r, "reconcile: not yet on-chain, re-queued")
                        .await?
                }
            }
        }
        Ok(())
    }

    async fn nonce(&mut self) -> anyhow::Result<u64> {
        if let Some(n) = self.next_nonce {
            return Ok(n);
        }
        let n = self
            .chain
            .online()
            .tx()
            .await?
            .account_nonce(&self.signer_account)
            .await?;
        self.next_nonce = Some(n);
        Ok(n)
    }

    /// Log one `attester_resources` line and warn below the configured floors.
    ///
    /// Under proxy signing the two resources sit on different accounts: the
    /// allowance on the primary, the transaction fee on the signer.
    async fn log_attester_resources(&self) -> anyhow::Result<()> {
        let allowance_account = self.config.proxy_for.unwrap_or(self.signer_account.0);
        let allowance = self.chain.attestation_allowance(allowance_account).await?;
        let signer_balance = self.chain.free_balance(self.signer_account.0).await?;
        let primary_balance = match self.config.proxy_for {
            Some(primary) => Some(self.chain.free_balance(primary).await?),
            None => None,
        };

        tracing::info!(
            allowance,
            allowance_account = %hex_account(&allowance_account),
            signer = %hex_account(&self.signer_account.0),
            signer_balance_planck = signer_balance,
            primary_balance_planck = primary_balance,
            "attester_resources"
        );
        metrics::gauge!("ibv2_attester_allowance").set(allowance as f64);
        metrics::gauge!("ibv2_account_free_balance_planck", "role" => "signer")
            .set(signer_balance as f64);
        if let Some(primary) = primary_balance {
            metrics::gauge!("ibv2_account_free_balance_planck", "role" => "primary")
                .set(primary as f64);
        }

        if allowance < self.config.allowance_floor {
            tracing::warn!(
                allowance,
                floor = self.config.allowance_floor,
                allowance_account = %hex_account(&allowance_account),
                "attestation allowance below floor; registration stops at zero"
            );
        }
        if signer_balance < self.config.signer_balance_floor_planck {
            tracing::warn!(
                signer_balance_planck = signer_balance,
                floor_planck = self.config.signer_balance_floor_planck,
                signer = %hex_account(&self.signer_account.0),
                "chain-writer signer balance below floor; registrations will fail to pay fees"
            );
        }
        Ok(())
    }

    async fn assign(&self, guard: &Guard, r: &Reservation) -> anyhow::Result<()> {
        if !outbox::mark_assigned(&self.pool, guard, r.id).await? {
            anyhow::bail!("lease lost while assigning");
        }
        tracing::info!(id = r.id, username = %r.full_username, "registration assigned on-chain");
        Ok(())
    }

    async fn fail(&self, guard: &Guard, r: &Reservation, reason: &str) -> anyhow::Result<()> {
        if !outbox::mark_failed(&self.pool, guard, r.id, reason).await? {
            anyhow::bail!("lease lost while failing");
        }
        tracing::warn!(id = r.id, username = %r.full_username, reason, "registration failed terminally");
        Ok(())
    }

    async fn retry(&self, guard: &Guard, r: &Reservation, reason: &str) -> anyhow::Result<()> {
        let attempt = r.attempt + 1;
        let backoff = 2u64.saturating_pow(attempt.clamp(0, 6) as u32);
        let not_before = OffsetDateTime::now_utc() + time::Duration::seconds(backoff as i64);
        if !outbox::mark_retry(&self.pool, guard, r.id, not_before, attempt, reason).await? {
            anyhow::bail!("lease lost while scheduling retry");
        }
        tracing::warn!(
            id = r.id,
            attempt,
            backoff_secs = backoff,
            reason,
            "registration retry scheduled"
        );
        Ok(())
    }
}

/// Record depth and oldest-row age per outbox status — where "the oldest queued
/// claim" comes from. Read-only; the writer owns it as the always-on process
/// next to the outbox.
async fn record_outbox_gauges(pool: &PgPool) -> Result<(), sqlx::Error> {
    for (status, depth) in outbox::depth_by_status(pool).await? {
        let status = status.as_str();
        metrics::gauge!("ibv2_outbox_depth", "status" => status).set(depth.depth as f64);
        metrics::gauge!("ibv2_outbox_oldest_age_seconds", "status" => status)
            .set(depth.oldest_age_secs.unwrap_or(0.0));
    }
    Ok(())
}

/// Build the registration extrinsic: `PeopleLite.attest`, optionally wrapped in
/// `Proxy.proxy(real = attester authority, …)`.
fn build_registration_tx(
    r: &Reservation,
    candidate: &[u8; 32],
    proxy_for: Option<&[u8; 32]>,
) -> DynamicPayload<Vec<Value>> {
    match proxy_for {
        Some(real) => {
            let args = vec![
                // real: MultiAddress::Id(attester authority)
                Value::unnamed_variant("Id", [Value::from_bytes(real)]),
                // force_proxy_type: Option<ProxyType> = None (any granted type)
                Value::unnamed_variant("None", []),
                attest_call(r, candidate),
            ];
            dynamic::tx("Proxy", "proxy", args)
        }
        None => dynamic::tx("PeopleLite", "attest", attest_args(r, candidate)),
    }
}

/// One `PeopleLite.attest` call as a `RuntimeCall` value.
fn attest_call(r: &Reservation, candidate: &[u8; 32]) -> Value {
    Value::unnamed_variant(
        "PeopleLite",
        [Value::unnamed_variant("attest", attest_args(r, candidate))],
    )
}

fn attest_args(r: &Reservation, candidate: &[u8; 32]) -> Vec<Value> {
    let reserved_username = match &r.reserved_username {
        Some(name) => Value::unnamed_variant("Some", [Value::from_bytes(name.as_bytes())]),
        None => Value::unnamed_variant("None", []),
    };
    let consumer = Value::named_composite(vec![
        (
            "signature".to_string(),
            sr25519_signature(&r.consumer_registration_signature),
        ),
        ("account".to_string(), Value::from_bytes(candidate)),
        (
            "identifier_key".to_string(),
            Value::from_bytes(&r.identifier_key),
        ),
        (
            "username".to_string(),
            Value::from_bytes(r.full_username.as_bytes()),
        ),
        ("reserved_username".to_string(), reserved_username),
    ]);
    vec![
        Value::from_bytes(candidate),
        sr25519_signature(&r.candidate_signature),
        Value::from_bytes(&r.ring_vrf_key),
        Value::from_bytes(&r.proof_of_ownership),
        Value::unnamed_variant("Some", [consumer]),
    ]
}

/// `MultiSignature::Sr25519(sig)`.
fn sr25519_signature(bytes: &[u8]) -> Value {
    Value::unnamed_variant("Sr25519", [Value::from_bytes(bytes)])
}

fn parse_account(ss58: &str) -> anyhow::Result<[u8; 32]> {
    Ok(AccountId32::from_str(ss58)
        .map_err(|e| anyhow::anyhow!("invalid SS58 account {ss58}: {e}"))?
        .0)
}

fn hex_account(bytes: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation() -> Reservation {
        Reservation {
            id: 1,
            full_username: "testing.42".to_string(),
            candidate_account_id: String::new(),
            candidate_signature: vec![1; 64],
            ring_vrf_key: vec![2; 32],
            proof_of_ownership: vec![3; 64],
            consumer_registration_signature: vec![4; 64],
            identifier_key: vec![5; 65],
            reserved_username: None,
            attempt: 0,
        }
    }

    #[test]
    fn direct_registration_is_not_wrapped_in_utility_batch() {
        let reservation = reservation();
        let candidate = [7; 32];
        let payload = build_registration_tx(&reservation, &candidate, None);

        assert_eq!(payload.pallet_name(), "PeopleLite");
        assert_eq!(payload.call_name(), "attest");
        assert_eq!(payload.call_data(), &attest_args(&reservation, &candidate));
    }

    #[test]
    fn proxied_registration_wraps_attest_directly() {
        let reservation = reservation();
        let candidate = [7; 32];
        let proxy_for = [8; 32];
        let payload = build_registration_tx(&reservation, &candidate, Some(&proxy_for));

        assert_eq!(payload.pallet_name(), "Proxy");
        assert_eq!(payload.call_name(), "proxy");
        assert_eq!(
            payload.call_data()[2],
            attest_call(&reservation, &candidate)
        );
    }

    #[test]
    fn failed_attest_retries_then_fails_without_becoming_assigned() {
        let candidate = [7; 32];
        let reason = "PeopleLite.InvalidAttestationSignature";

        assert_eq!(
            classify_submit_failure(reason, None, candidate, 1, 3),
            SubmitFailureAction::Retry
        );
        assert_eq!(
            classify_submit_failure(reason, None, candidate, 3, 3),
            SubmitFailureAction::Fail
        );
    }

    #[test]
    fn submit_error_assigns_only_after_successful_reconciliation() {
        let candidate = [7; 32];

        assert_eq!(
            classify_submit_failure("finalization timed out", Some(candidate), candidate, 1, 3),
            SubmitFailureAction::Assign
        );
        assert_eq!(
            classify_submit_failure("AlreadyRegistered", None, candidate, 1, 3),
            SubmitFailureAction::Assign
        );
        assert_eq!(
            classify_submit_failure("dispatch failed", Some([8; 32]), candidate, 1, 3),
            SubmitFailureAction::Retry
        );
    }
}
