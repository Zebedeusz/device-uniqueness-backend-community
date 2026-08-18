//! `device-attestation-chain-writer` — the single-instance worker that drains the reservation
//! outbox onto People Chain.
//!
//! Separate from the N-replica `device-attestation-api`: exactly one live signer per
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
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chain_types::people::runtime_types::sp_runtime::DispatchError;
use chain_types::{
    people, AssetHubConfig, AssetHubExtrinsicParamsBuilder, PeopleConfig,
    PeopleExtrinsicParamsBuilder,
};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::PgPool;
use subxt::dynamic::{self, Value};
use subxt::extrinsics::ExtrinsicEvents;
use subxt::tx::{DynamicPayload, TransactionStatus};
use subxt::utils::AccountId32;
use time::OffsetDateTime;

use chain_client::WriterSigner;

use super::asset_hub::{AssetHubClient, ValidityWindow};
use super::client::ChainClient;
use super::lease;
use super::outbox::{self, Guard, Reservation};
use crate::dotns;

/// Chain-writer configuration, loaded from the environment.
#[derive(Debug, Clone)]
pub struct WriterConfig {
    /// Postgres connection string (shared schema with device-attestation-api). May embed a
    /// password, so it is a secret.
    pub database_url: SecretString,
    /// People Chain RPC endpoint.
    pub people_rpc_url: String,
    /// SURI of the hot signing key (the proxy key, or the primary in dev).
    pub signer_suri: SecretString,
    /// Attester authority (`ATTESTER_ACCOUNT`), the account device-attestation-api also
    /// publishes. Proxying is derived from it — see
    /// [`WriterSigner::proxy_for`].
    pub attester: [u8; 32],
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
    /// match device-attestation-api's value). On: `QUEUED` rows are never drained here;
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
    /// Whether the dotNS gateway lane is live (`DOTNS_GATEWAY_ENABLED`). Must
    /// match device-attestation-api's value, which gates intake. When off, no Asset Hub
    /// connection is opened at all and `dotns_status` rows are left alone.
    pub dotns_gateway_enabled: bool,
    /// Asset Hub RPC endpoint. Required when the dotNS lane is enabled.
    pub asset_hub_rpc_url: Option<String>,
}

impl WriterConfig {
    /// Read and validate writer configuration from the environment.
    pub fn from_env() -> anyhow::Result<Self> {
        // Namespaced: this worker shares its sibling API's database, and a bare
        // DATABASE_URL would mean four different instances across the workspace.
        let database_url = http_common::config::required_var("DEVICE_ATTESTATION_DATABASE_URL")?;
        let people_rpc_url = std::env::var("PEOPLE_RPC_URL")
            .unwrap_or_else(|_| "wss://previewnet.substrate.dev/people".to_string());
        let signer_suri = std::env::var("CHAIN_WRITER_SIGNER_SURI")
            .context("CHAIN_WRITER_SIGNER_SURI is required")?;
        let attester = crate::config::attester_account_from_env()?;
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
            attester,
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
            // Matches device-attestation-api's intake gate (`config.rs`). Absent on one
            // side and set on the other is the accepted-but-never-submitted
            // split; the same default on both is what stops it. `.env.example`
            // and `docker-compose.yml` ship the lane **on** — see the rationale
            // on `Config::from_env`'s `dotns_gateway_enabled`.
            dotns_gateway_enabled: crate::config::env_bool("DOTNS_GATEWAY_ENABLED", false)?,
            asset_hub_rpc_url: match std::env::var("ASSET_HUB_RPC_URL") {
                Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
                _ => None,
            },
        })
    }
}

/// Connect and run the writer until fatal error (the process is expected to be
/// supervised and restarted).
pub async fn run(config: WriterConfig) -> anyhow::Result<()> {
    tracing::info!(
        people_rpc = %config.people_rpc_url,
        attester = %hex_account(&config.attester),
        holder = %config.holder_id,
        "starting device-attestation-chain-writer"
    );
    let pool = crate::db::connect(config.database_url.expose_secret()).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    let signer = WriterSigner::from_secret(config.signer_suri.expose_secret())?;
    let signer_account = AccountId32(signer.public_bytes());
    let proxy_for = signer
        .proxy_for(AccountId32(config.attester))
        .map(|primary| primary.0);

    let dotns_lane = match (config.dotns_gateway_enabled, &config.asset_hub_rpc_url) {
        (true, Some(url)) => {
            tracing::info!(
                asset_hub_rpc = %url,
                "dotns lane enabled; Asset Hub connects on the first pass"
            );
            DotnsLane::Enabled {
                rpc_url: url.clone(),
                connected: None,
                last_error: None,
                retry_at: None,
            }
        }
        // A config error, not a chain one: the two processes disagree about
        // whether the lane exists. Nothing a retry fixes, and it is knowable
        // before a single row is claimed, so it stays a startup abort.
        (true, None) => anyhow::bail!(
            "DOTNS_GATEWAY_ENABLED is on but ASSET_HUB_RPC_URL is unset. device-attestation-api would \
             accept dotns blocks that nothing ever submits — set the RPC URL, or turn the \
             gateway off for this environment."
        ),
        (false, _) => {
            tracing::info!("dotns lane disabled");
            DotnsLane::Disabled
        }
    };
    tracing::info!(
        signer = %hex_account(&signer_account.0),
        attester = %hex_account(&config.attester),
        mode = if proxy_for.is_some() { "proxy" } else { "direct" },
        "device-attestation-chain-writer connected"
    );
    record_writer_info(&config, &signer_account);
    zero_init_submit_outcomes();
    // No HTTP surface here, so this is the only readiness signal it has.
    http_common::metrics::spawn_readiness_probe(
        "device-attestation-chain-writer",
        (pool.clone(), chain.clone()),
        |(p, c)| crate::http::health::probe(p, c),
    );

    let mut writer = Writer {
        pool,
        chain,
        dotns_lane,
        signer,
        signer_account,
        proxy_for,
        config,
        next_nonce: None,
        next_nonce_ah: None,
    };
    writer.run_forever().await
}

struct Writer {
    pool: PgPool,
    chain: ChainClient,
    /// The dotNS lane's Asset Hub connection. See [`DotnsLane`].
    dotns_lane: DotnsLane,
    signer: WriterSigner,
    signer_account: AccountId32,
    /// Derived once at connect time by [`WriterSigner::proxy_for`].
    proxy_for: Option<[u8; 32]>,
    config: WriterConfig,
    /// Locally-tracked People nonce; reset to a chain re-fetch on any error.
    next_nonce: Option<u64>,
    /// Locally-tracked Asset Hub nonce. A **separate lane**. Nonces are per
    /// chain, so one account can have one in flight on each. Same reset
    /// discipline as `next_nonce`.
    next_nonce_ah: Option<u64>,
}

/// How long the lane waits before retrying a failed Asset Hub connect. The
/// dotNS pass runs on `CHAIN_WRITER_POLL_SECS` (2s by default) and a connect
/// costs a metadata fetch, so a down endpoint is not dialled once per pass.
const DOTNS_RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// The dotNS lane's Asset Hub connection, opened on demand.
///
/// Deliberately **not** opened at startup. Every post-startup Asset Hub error is
/// already demoted to a per-row `RETRY_AFTER` precisely so the dotNS half can
/// never affect the People half — but a boot-time one used to be the exception
/// that propagated out of `run`, crash-looping the process and stalling People
/// registrations entirely. An Asset Hub that is merely unreachable when the
/// writer happens to restart (an RPC bounce, a DNS blip, PreviewNet
/// maintenance) is not a reason to stop attesting usernames.
///
/// The `reserve_name` shape assertion lives on this path too, and it is not a
/// startup-only risk: a runtime upgrade can start failing it at any time. Both
/// failures now park the dotNS lane, leaving rows in `PENDING` for whenever the
/// endpoint comes back, and leave People running.
enum DotnsLane {
    /// `DOTNS_GATEWAY_ENABLED` is off. No connection is ever opened.
    Disabled,
    /// Enabled, with the endpoint to reach.
    Enabled {
        rpc_url: String,
        /// `None` until the first successful connect.
        connected: Option<(AssetHubClient, ValidityWindow)>,
        /// The last connect failure, so a lastingly unreachable or mis-shaped
        /// Asset Hub logs once per distinct cause rather than once per pass.
        last_error: Option<String>,
        /// Earliest next connect attempt, once one has failed.
        retry_at: Option<Instant>,
    },
}

/// What the lane wants done before the dotNS half of a pass can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DotnsDial {
    /// Already connected. Use the cached client.
    Ready,
    /// Not connected and the backoff has elapsed. Dial.
    Dial,
    /// Skip the dotNS half this pass: the lane is off, or the backoff has not
    /// elapsed since the last failed dial.
    Skip,
}

impl DotnsLane {
    /// Whether to dial, reuse, or skip — the whole backoff decision, kept pure
    /// so it can be tested without a chain.
    fn dial_state(&self, now: Instant) -> DotnsDial {
        match self {
            DotnsLane::Disabled => DotnsDial::Skip,
            DotnsLane::Enabled { connected, .. } if connected.is_some() => DotnsDial::Ready,
            DotnsLane::Enabled { retry_at, .. } => match retry_at {
                Some(at) if now < *at => DotnsDial::Skip,
                _ => DotnsDial::Dial,
            },
        }
    }

    /// Records a failed dial and reports whether this cause is new.
    ///
    /// `false` means an identical failure was already logged, so the caller
    /// stays quiet: a shape mismatch is not transient and would otherwise print
    /// every backoff interval, forever.
    fn record_dial_failure(&mut self, reason: String, now: Instant) -> bool {
        let DotnsLane::Enabled {
            last_error,
            retry_at,
            ..
        } = self
        else {
            return false;
        };
        *retry_at = Some(now + DOTNS_RECONNECT_INTERVAL);
        let is_new = last_error.as_deref() != Some(reason.as_str());
        if is_new {
            *last_error = Some(reason);
        }
        is_new
    }

    /// Records a successful dial, clearing the parked state.
    fn record_dial_success(&mut self, up: (AssetHubClient, ValidityWindow)) {
        if let DotnsLane::Enabled {
            connected,
            last_error,
            retry_at,
            ..
        } = self
        {
            *connected = Some(up);
            *last_error = None;
            *retry_at = None;
        }
    }
}

/// Connects and reads the gateway's freshness constants.
///
/// [`AssetHubClient::connect`] asserts the runtime's `reserve_name` argument
/// names, so a chain running the other variant fails here rather than
/// mid-submit.
async fn connect_asset_hub(url: &str) -> anyhow::Result<(AssetHubClient, ValidityWindow)> {
    let client = AssetHubClient::connect(url).await?;
    let window = client.validity_window().await?;
    Ok((client, window))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitFailureAction {
    Assign,
    Retry,
    Fail,
}

/// Why a row cannot be submitted to Asset Hub as it stands.
///
/// All of them would otherwise cost a fee to discover on chain. Every variant
/// is terminal except [`DotnsReject::FutureDated`], which the passage of time
/// resolves on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DotnsReject {
    /// The row carries no dotns block, or an incomplete one.
    NotInLane,
    /// The lite label is not `<stem>.<digits>`, or it exceeds `BaseLabel`'s
    /// 32-byte bound.
    UnbuildableLabel(String),
    /// `reservedUsername` exceeds `BaseLabel`'s 32-byte bound.
    UnbuildableReserved(String),
    /// The stored signature does not verify against the reservation payload.
    BadSignature,
    /// `signed_at` aged out of the pallet's validity window.
    Expired { signed_at: i64, deadline_secs: u64 },
    /// `signed_at` is further ahead of the chain's clock than
    /// `MaxFutureSkewSeconds` allows.
    ///
    /// The only non-terminal reject. The row becomes submittable at
    /// `submittable_at` with no client involvement, so it is deferred rather
    /// than failed.
    FutureDated { signed_at: i64, submittable_at: i64 },
}

/// `BaseLabel` is `BoundedVec<u8, ConstU32<32>>` in the gateway pallet.
const MAX_LABEL_BYTES: usize = 32;

/// Runs every offline gate over a row before it costs an extrinsic.
///
/// Ordered cheapest-first, though all of them are pure. The expensive
/// `LiteLabelOwner` read happens only after this returns `Ok`.
fn check_dotns_submittable(
    r: &Reservation,
    candidate: &[u8; 32],
    attester: &[u8; 32],
    window: ValidityWindow,
    now: i64,
) -> Result<(), DotnsReject> {
    let (Some(signature), Some(signed_at)) = (&r.dotns_signature, r.dotns_signed_at) else {
        return Err(DotnsReject::NotInLane);
    };

    let label = &r.full_username;
    if label.len() > MAX_LABEL_BYTES {
        return Err(DotnsReject::UnbuildableLabel(format!(
            "lite label is {} bytes, over BaseLabel's {MAX_LABEL_BYTES}",
            label.len()
        )));
    }
    let base = dotns::lite_base(label);
    if base.len() == label.len() {
        return Err(DotnsReject::UnbuildableLabel(
            "lite label has no digit suffix".to_string(),
        ));
    }
    if let Some(reserved) = &r.reserved_username {
        if reserved.len() > MAX_LABEL_BYTES {
            return Err(DotnsReject::UnbuildableReserved(format!(
                "reservedUsername is {} bytes, over BaseLabel's {MAX_LABEL_BYTES}",
                reserved.len()
            )));
        }
    }

    // `reserve_name` bounds `signed_at` on both sides. Intake has its own
    // DOTNS_MAX_FUTURE_SKEW_SECS, but that is a separate value on a separate
    // process: if it drifts above the pallet's, or a row reaches the lane
    // through a path configured differently, a future-dated signature would
    // sail through every other gate and burn a fee on a call the pallet
    // rejects. This is the only place the chain's own bound is enforced.
    let max_future_skew = i64::try_from(window.max_future_skew_secs).unwrap_or(i64::MAX);
    if signed_at > now.saturating_add(max_future_skew) {
        return Err(DotnsReject::FutureDated {
            signed_at,
            submittable_at: signed_at.saturating_sub(max_future_skew),
        });
    }

    // Checking expiry before signature verification. An aged-out row is EXPIRED
    // even if its signature is also wrong. That is the state the client can act
    // on by re-signing, rather than a bug report.
    if dotns::reservation_expired(signed_at, window.max_validity_secs, now) {
        return Err(DotnsReject::Expired {
            signed_at,
            deadline_secs: window.max_validity_secs,
        });
    }

    let signed_at = u64::try_from(signed_at).map_err(|_| DotnsReject::BadSignature)?;
    if !dotns::verify_reservation_signature(
        signature,
        candidate,
        attester,
        base.as_bytes(),
        &r.identifier_key,
        r.reserved_username.as_ref().map(|s| s.as_bytes()),
        signed_at,
    ) {
        return Err(DotnsReject::BadSignature);
    }
    Ok(())
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
            self.next_nonce_ah = None;
            if let Err(e) = self.reconcile_submitting(&guard).await {
                tracing::warn!(error = %e, "startup reconcile failed");
            }
            if let Err(e) = self.reconcile_dotns_submitting(&guard).await {
                tracing::warn!(error = %e, "startup dotns reconcile failed");
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
            for r in &due {
                // Renew before each row: a batch drains sequentially and each
                // submit awaits finalization, so a batch can outlast the lease TTL.
                if !self.heartbeat(guard).await? {
                    anyhow::bail!("lost writer lease");
                }
                self.process(guard, r).await?;
            }
            // Asset Hub second. A row is only eligible once People has assigned
            // it. This picks up both the batch just drained and anything a
            // previous pass left in RETRY_AFTER.
            self.dotns_pass(guard).await?;
            if due.is_empty() {
                tokio::time::sleep(self.config.poll_interval).await;
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

        let payload = build_registration_tx(r, &candidate, self.proxy_for.as_ref());
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

        let metadata = chain_types::metadata();
        let progress = signed.submit_and_watch().await?;
        let wait = async {
            let mut progress = progress;
            loop {
                match progress.next().await {
                    Some(status) => match status? {
                        TransactionStatus::InFinalizedBlock(in_block) => {
                            let events = in_block.wait_for_success().await?;
                            check_proxied_call(&events, metadata)?;
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

    /// The Asset Hub nonce. A lane of its own. Nonces are per chain, so this
    /// never interacts with `next_nonce`.
    async fn nonce_ah(&mut self, asset_hub: &AssetHubClient) -> anyhow::Result<u64> {
        if let Some(n) = self.next_nonce_ah {
            return Ok(n);
        }
        let n = asset_hub
            .online()
            .tx()
            .await?
            .account_nonce(&self.signer_account)
            .await?;
        self.next_nonce_ah = Some(n);
        Ok(n)
    }

    /// The connected Asset Hub, dialling it if the lane is enabled and not yet
    /// up.
    ///
    /// `None` means the dotNS half is skipped this pass: the lane is off, or a
    /// connect failed and the backoff has not elapsed. Deliberately returns no
    /// error — nothing on this path may propagate into the People lane.
    async fn dotns_client(&mut self) -> Option<(AssetHubClient, ValidityWindow)> {
        let now = Instant::now();
        match self.dotns_lane.dial_state(now) {
            DotnsDial::Skip => return None,
            DotnsDial::Ready => {
                let DotnsLane::Enabled {
                    connected: Some(up),
                    ..
                } = &self.dotns_lane
                else {
                    return None;
                };
                return Some(up.clone());
            }
            DotnsDial::Dial => {}
        }
        let DotnsLane::Enabled { rpc_url, .. } = &self.dotns_lane else {
            return None;
        };
        let rpc_url = rpc_url.clone();
        match connect_asset_hub(&rpc_url).await {
            Ok(up) => {
                tracing::info!(
                    asset_hub_rpc = %rpc_url,
                    max_validity_secs = up.1.max_validity_secs,
                    max_future_skew_secs = up.1.max_future_skew_secs,
                    "dotns lane connected"
                );
                metrics::gauge!("dub_dotns_lane_connected").set(1.0);
                self.dotns_lane.record_dial_success(up.clone());
                Some(up)
            }
            Err(e) => {
                let reason = format!("{e:#}");
                if self.dotns_lane.record_dial_failure(reason.clone(), now) {
                    tracing::warn!(
                        asset_hub_rpc = %rpc_url,
                        error = %reason,
                        retry_secs = DOTNS_RECONNECT_INTERVAL.as_secs(),
                        "dotns lane parked; People registration is unaffected"
                    );
                }
                metrics::gauge!("dub_dotns_lane_connected").set(0.0);
                None
            }
        }
    }

    /// Drains rows due for Asset Hub reservation.
    ///
    /// Only rows already `ASSIGNED` on People are eligible. This can never
    /// claim a label for a username that failed to register.
    async fn dotns_pass(&mut self, guard: &Guard) -> anyhow::Result<()> {
        let Some((asset_hub, window)) = self.dotns_client().await else {
            return Ok(());
        };
        let due = outbox::claim_dotns_due(&self.pool, self.config.batch_size).await?;
        for r in &due {
            if !self.heartbeat(guard).await? {
                anyhow::bail!("lost writer lease");
            }
            self.process_dotns(guard, &asset_hub, window, r).await?;
        }
        Ok(())
    }

    /// Processes one reservation's Asset Hub half.
    ///
    /// Row-level failures are recorded on `dotns_status`. They never touch the
    /// People `status`.
    async fn process_dotns(
        &mut self,
        guard: &Guard,
        asset_hub: &AssetHubClient,
        window: ValidityWindow,
        r: &Reservation,
    ) -> anyhow::Result<()> {
        let Some(candidate) = parse_account(&r.candidate_account_id).ok() else {
            return self.dotns_fail(guard, r, "invalid candidate SS58").await;
        };
        let now = OffsetDateTime::now_utc().unix_timestamp();

        // Offline gates first. Each one is a fee not spent on a doomed call.
        if let Err(reject) =
            check_dotns_submittable(r, &candidate, &self.config.attester, window, now)
        {
            return match reject {
                DotnsReject::Expired {
                    signed_at,
                    deadline_secs,
                } => {
                    self.dotns_expire(
                        guard,
                        r,
                        &format!(
                            "reservation signature expired: signed_at={signed_at}, window \
                             {deadline_secs}s, now={now}. Only the client can re-sign."
                        ),
                    )
                    .await
                }
                DotnsReject::NotInLane => {
                    self.dotns_fail(guard, r, "row has no complete dotns block")
                        .await
                }
                DotnsReject::BadSignature => {
                    self.dotns_fail(guard, r, "dotns signature does not verify")
                        .await
                }
                DotnsReject::UnbuildableLabel(why) | DotnsReject::UnbuildableReserved(why) => {
                    self.dotns_fail(guard, r, &why).await
                }
                DotnsReject::FutureDated {
                    signed_at,
                    submittable_at,
                } => {
                    let until = OffsetDateTime::from_unix_timestamp(submittable_at)
                        .unwrap_or_else(|_| OffsetDateTime::now_utc());
                    self.dotns_defer(
                        guard,
                        r,
                        until,
                        &format!(
                            "reservation signature is future-dated: signed_at={signed_at}, \
                             now={now}, gateway tolerates {}s of skew. Re-queued until \
                             {submittable_at}.",
                            window.max_future_skew_secs
                        ),
                    )
                    .await
                }
            };
        }

        // Checking idempotency. The label may already be claimed.
        match asset_hub.lite_label_owner(&r.full_username).await {
            Ok(Some(owner)) if owner == candidate => return self.dotns_reserve(guard, r).await,
            Ok(Some(_)) => {
                return self
                    .dotns_fail(guard, r, "lite label reserved by another account")
                    .await
            }
            Ok(None) => {}
            Err(e) => {
                return self
                    .dotns_retry(guard, r, &format!("label owner read failed: {e}"))
                    .await
            }
        }

        let payload = build_reserve_name_tx(r, &candidate, self.proxy_for.as_ref());
        let nonce = match self.nonce_ah(asset_hub).await {
            Ok(n) => n,
            Err(e) => {
                return self
                    .dotns_retry(guard, r, &format!("asset hub nonce fetch: {e}"))
                    .await
            }
        };

        match self
            .submit_dotns(guard, asset_hub, r, &payload, nonce)
            .await
        {
            Ok(()) => {
                self.next_nonce_ah = Some(nonce + 1);
                self.dotns_reserve(guard, r).await
            }
            Err(e) => {
                self.next_nonce_ah = None;
                let reason = e.to_string();
                let observed = asset_hub
                    .lite_label_owner(&r.full_username)
                    .await
                    .ok()
                    .flatten();
                match classify_submit_failure(
                    &reason,
                    observed,
                    candidate,
                    r.dotns_attempt + 1,
                    self.config.max_attempts,
                ) {
                    SubmitFailureAction::Assign => self.dotns_reserve(guard, r).await,
                    SubmitFailureAction::Retry => self.dotns_retry(guard, r, &reason).await,
                    SubmitFailureAction::Fail => {
                        self.dotns_fail(guard, r, &format!("max attempts reached: {reason}"))
                            .await
                    }
                }
            }
        }
    }

    /// Signs and submits on Asset Hub.
    ///
    /// Records `SUBMITTING` before awaiting inclusion, so a crash mid-flight
    /// reconciles instead of resubmitting.
    async fn submit_dotns(
        &self,
        guard: &Guard,
        asset_hub: &AssetHubClient,
        r: &Reservation,
        payload: &DynamicPayload<Vec<Value>>,
        nonce: u64,
    ) -> anyhow::Result<()> {
        let params = AssetHubExtrinsicParamsBuilder::<AssetHubConfig>::new()
            .nonce(nonce)
            .build();
        let mut tx_client = asset_hub.online().tx().await?;
        let signed = tx_client
            .create_signed(payload, &self.signer, params)
            .await?;
        let tx_hash = format!("{:?}", signed.hash());

        if !outbox::mark_dotns_submitting(&self.pool, guard, r.id, &tx_hash, r.dotns_attempt + 1)
            .await?
        {
            anyhow::bail!("lease lost before dotns submit");
        }
        tracing::info!(
            id = r.id,
            username = %r.full_username,
            nonce,
            tx = %tx_hash,
            "submitting dotns reservation"
        );

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
        let watched = async move {
            tokio::pin!(wait);
            let mut renew = tokio::time::interval(self.config.lease_ttl / 3);
            renew.tick().await;
            loop {
                tokio::select! {
                    result = &mut wait => return result,
                    _ = renew.tick() => {
                        if !self.heartbeat(guard).await? {
                            anyhow::bail!("lost writer lease during dotns submit");
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

    /// Reconciles dotNS rows left in `SUBMITTING` by a previous writer.
    ///
    /// Skipped while the lane is parked. Those rows keep their `SUBMITTING`
    /// status and are reconciled by the next lease acquisition that finds Asset
    /// Hub reachable — never resubmitted blind.
    async fn reconcile_dotns_submitting(&mut self, guard: &Guard) -> anyhow::Result<()> {
        let Some((asset_hub, _)) = self.dotns_client().await else {
            return Ok(());
        };
        for r in outbox::dotns_submitting(&self.pool).await? {
            let Some(candidate) = parse_account(&r.candidate_account_id).ok() else {
                self.dotns_fail(guard, &r, "invalid candidate SS58").await?;
                continue;
            };
            match asset_hub.lite_label_owner(&r.full_username).await? {
                Some(owner) if owner == candidate => self.dotns_reserve(guard, &r).await?,
                _ => {
                    self.dotns_retry(guard, &r, "reconcile: not yet on Asset Hub, re-queued")
                        .await?
                }
            }
        }
        Ok(())
    }

    async fn dotns_reserve(&self, guard: &Guard, r: &Reservation) -> anyhow::Result<()> {
        if !outbox::mark_dotns_reserved(&self.pool, guard, r.id).await? {
            anyhow::bail!("lease lost while reserving dotns name");
        }
        record_submit_outcome("dotns", "ok");
        tracing::info!(id = r.id, username = %r.full_username, "dotns reserved on-chain");
        Ok(())
    }

    async fn dotns_fail(&self, guard: &Guard, r: &Reservation, reason: &str) -> anyhow::Result<()> {
        if !outbox::mark_dotns_failed(&self.pool, guard, r.id, reason).await? {
            anyhow::bail!("lease lost while failing dotns reservation");
        }
        record_submit_outcome("dotns", "terminal");
        tracing::warn!(
            id = r.id,
            username = %r.full_username,
            reason,
            "dotns reservation failed terminally; the People registration is unaffected"
        );
        Ok(())
    }

    async fn dotns_expire(
        &self,
        guard: &Guard,
        r: &Reservation,
        reason: &str,
    ) -> anyhow::Result<()> {
        if !outbox::mark_dotns_expired(&self.pool, guard, r.id, reason).await? {
            anyhow::bail!("lease lost while expiring dotns reservation");
        }
        record_submit_outcome("dotns", "terminal");
        tracing::warn!(
            id = r.id,
            username = %r.full_username,
            reason,
            "dotns reservation signature expired before submission"
        );
        Ok(())
    }

    /// Re-queues a row that is not yet submittable, **without** spending an
    /// attempt.
    ///
    /// Distinct from [`Writer::dotns_retry`]: nothing failed. A future-dated
    /// `signed_at` becomes valid as the clock advances, so counting it against
    /// `CHAIN_WRITER_MAX_ATTEMPTS` would eventually fail a row that was always
    /// going to succeed.
    async fn dotns_defer(
        &self,
        guard: &Guard,
        r: &Reservation,
        until: OffsetDateTime,
        reason: &str,
    ) -> anyhow::Result<()> {
        if !outbox::mark_dotns_retry(&self.pool, guard, r.id, until, r.dotns_attempt, reason)
            .await?
        {
            anyhow::bail!("lease lost while deferring dotns reservation");
        }
        tracing::warn!(
            id = r.id,
            username = %r.full_username,
            until = %until,
            reason,
            "dotns reservation deferred; not yet within the gateway's skew bound"
        );
        Ok(())
    }

    async fn dotns_retry(
        &self,
        guard: &Guard,
        r: &Reservation,
        reason: &str,
    ) -> anyhow::Result<()> {
        let attempt = r.dotns_attempt + 1;
        let backoff = 2u64.saturating_pow(attempt.clamp(0, 6) as u32);
        let not_before = OffsetDateTime::now_utc() + time::Duration::seconds(backoff as i64);
        if !outbox::mark_dotns_retry(&self.pool, guard, r.id, not_before, attempt, reason).await? {
            anyhow::bail!("lease lost while scheduling dotns retry");
        }
        record_submit_outcome("dotns", "retry");
        tracing::warn!(
            id = r.id,
            attempt,
            backoff_secs = backoff,
            reason,
            "dotns reservation retry scheduled"
        );
        Ok(())
    }

    /// Log one `attester_resources` line and warn below the configured floors.
    ///
    /// Under proxy signing the two resources sit on different accounts: the
    /// allowance on the primary, the transaction fee on the signer.
    async fn log_attester_resources(&mut self) -> anyhow::Result<()> {
        let allowance_account = self.config.attester;
        let allowance = self.chain.attestation_allowance(allowance_account).await?;
        let signer_balance = self.chain.free_balance(self.signer_account.0).await?;
        let primary_balance = match self.proxy_for {
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
        metrics::gauge!("dub_attester_allowance").set(allowance as f64);
        record_spec_version("people", self.chain.online()).await;
        // `chain` is carried by every series in this family, People included.
        // Prometheus matchers are subset-based: without it, a selector written
        // as {role="signer"} would match the Asset Hub series too, so a
        // below-floor alert would fire on the wrong balance and any sum() over
        // it would silently double.
        metrics::gauge!(
            "dub_account_free_balance_planck",
            "role" => "signer",
            "chain" => "people"
        )
        .set(signer_balance as f64);
        if let Some(primary) = primary_balance {
            metrics::gauge!(
                "dub_account_free_balance_planck",
                "role" => "primary",
                "chain" => "people"
            )
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

        // The Asset Hub lane spends a second, independent budget. That is the
        // gateway's own allowance on the attester, plus fees on the signer.
        // reserve_name is Pays::No only when it succeeds. Skipped while the
        // lane is parked — this pass doubles as its reconnect attempt.
        if let Some((asset_hub, _)) = self.dotns_client().await {
            let allowance = asset_hub.attestation_allowance(allowance_account).await?;
            let ah_signer_balance = asset_hub.free_balance(self.signer_account.0).await?;
            tracing::info!(
                allowance,
                allowance_account = %hex_account(&allowance_account),
                signer_balance_planck = ah_signer_balance,
                "dotns_attester_resources"
            );
            metrics::gauge!("dub_dotns_attester_allowance").set(allowance as f64);
            record_spec_version("asset-hub", asset_hub.online()).await;
            metrics::gauge!(
                "dub_account_free_balance_planck",
                "role" => "signer",
                "chain" => "asset-hub"
            )
            .set(ah_signer_balance as f64);

            if allowance < self.config.allowance_floor {
                tracing::warn!(
                    allowance,
                    floor = self.config.allowance_floor,
                    allowance_account = %hex_account(&allowance_account),
                    "dotns gateway allowance below floor; reservations stop at zero"
                );
            }
            if ah_signer_balance < self.config.signer_balance_floor_planck {
                tracing::warn!(
                    signer_balance_planck = ah_signer_balance,
                    floor_planck = self.config.signer_balance_floor_planck,
                    signer = %hex_account(&self.signer_account.0),
                    "chain-writer signer balance on Asset Hub below floor; \
                     failed reservations will not pay their fees"
                );
            }
        }
        Ok(())
    }

    async fn assign(&self, guard: &Guard, r: &Reservation) -> anyhow::Result<()> {
        if !outbox::mark_assigned(&self.pool, guard, r.id).await? {
            anyhow::bail!("lease lost while assigning");
        }
        record_submit_outcome("people", "ok");
        tracing::info!(id = r.id, username = %r.full_username, "registration assigned on-chain");
        Ok(())
    }

    async fn fail(&self, guard: &Guard, r: &Reservation, reason: &str) -> anyhow::Result<()> {
        if !outbox::mark_failed(&self.pool, guard, r.id, reason).await? {
            anyhow::bail!("lease lost while failing");
        }
        record_submit_outcome("people", "terminal");
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
        record_submit_outcome("people", "retry");
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

/// Publish which accounts this deployment signs as, on a constant-1 series.
///
/// The first question in an incident is which account, and the answer otherwise
/// lives in one INFO line that ages out of Loki and in a `.env` on the server.
///
/// **Never add a credential, and never add a configured URL** — an RPC endpoint
/// can carry an API key in its path or userinfo, and this surface is read
/// anonymously.
fn record_writer_info(config: &WriterConfig, signer: &AccountId32) {
    metrics::gauge!(
        "dub_writer_info",
        "signer" => hex_account(&signer.0),
        "attester" => hex_account(&config.attester),
        "dotns_lane" => if config.dotns_gateway_enabled { "enabled" } else { "disabled" }
    )
    .set(1.0);
}

/// Publish the runtime version the client is currently signing against.
///
/// The value only matters when it *changes*: a runtime upgrade can turn a
/// working submission path into a failing one with no deploy of ours. Read at
/// the current finalized block, so it follows an upgrade rather than reporting
/// what was true at boot. Best-effort — a failed read leaves the last value.
async fn record_spec_version<C: subxt::Config>(
    chain: &'static str,
    client: &subxt::OnlineClient<C>,
) {
    match client.at_current_block().await {
        Ok(at) => {
            metrics::gauge!("dub_chain_spec_version", "chain" => chain)
                .set(at.spec_version() as f64);
            metrics::gauge!("dub_chain_transaction_version", "chain" => chain)
                .set(at.transaction_version() as f64);
        }
        Err(error) => {
            tracing::warn!(chain, %error, "reading the runtime version failed");
        }
    }
}

/// The lanes and outcomes [`record_submit_outcome`] can emit.
const SUBMIT_LANES: [&str; 2] = ["people", "dotns"];
const SUBMIT_OUTCOMES: [&str; 3] = ["ok", "retry", "terminal"];

/// Register every submission counter at zero, before anything is submitted.
///
/// The exporter only renders what has been recorded, so an untouched counter is
/// absent rather than zero — which a dashboard cannot tell from a broken query
/// or a dead process.
fn zero_init_submit_outcomes() {
    for lane in SUBMIT_LANES {
        for outcome in SUBMIT_OUTCOMES {
            metrics::counter!("dub_chain_submit_total", "lane" => lane, "outcome" => outcome)
                .absolute(0);
        }
    }
}

/// Count one terminal-for-now outcome of a chain submission.
///
/// The two lanes are counted separately because they fail for different reasons
/// and one is allowed to be parked while the other works. `outcome` is bounded
/// to three values; the reason stays in the logs, where it costs no cardinality.
///
/// Deferrals are not counted. Nothing failed — a future-dated `signed_at`
/// becomes valid on its own, and counting it would fake a failure rate.
fn record_submit_outcome(lane: &'static str, outcome: &'static str) {
    metrics::counter!("dub_chain_submit_total", "lane" => lane, "outcome" => outcome).increment(1);
}

/// Record depth and longest wait per outbox status — where "the oldest queued
/// claim" comes from. The age is time *in that status*, so it is comparable
/// across statuses and does not grow forever on the terminal ones. Read-only;
/// the writer owns it as the always-on process next to the outbox.
async fn record_outbox_gauges(pool: &PgPool) -> Result<(), sqlx::Error> {
    for (status, depth) in outbox::depth_by_status(pool).await? {
        let status = status.as_str();
        metrics::gauge!("dub_outbox_depth", "status" => status).set(depth.depth as f64);
        metrics::gauge!("dub_outbox_oldest_age_seconds", "status" => status)
            .set(depth.oldest_age_secs.unwrap_or(0.0));
    }
    // The Asset Hub lane's own depths. A separate series, because a row can
    // rest in ASSIGNED + DOTNS_FAILED_TERMINAL and one gauge cannot say both.
    for (status, depth) in outbox::dotns_depth_by_status(pool).await? {
        let status = status.as_str();
        metrics::gauge!("dub_dotns_outbox_depth", "status" => status).set(depth.depth as f64);
        metrics::gauge!("dub_dotns_outbox_oldest_age_seconds", "status" => status)
            .set(depth.oldest_age_secs.unwrap_or(0.0));
    }
    Ok(())
}

/// Fail a submit whose proxied call was rejected inside a successful
/// `Proxy.proxy` extrinsic.
///
/// A rejected inner call still emits `ExtrinsicSuccess`, so without this check
/// `wait_for_success` reports a registration that never landed as `ASSIGNED`.
fn check_proxied_call(
    events: &ExtrinsicEvents<PeopleConfig>,
    metadata: &subxt::Metadata,
) -> anyhow::Result<()> {
    for event in events.find::<people::proxy::events::ProxyExecuted>() {
        if let Err(error) = event.context("decoding ProxyExecuted")?.result {
            anyhow::bail!("proxied call failed: {}", describe(&error, metadata));
        }
    }
    Ok(())
}

/// Render a module `DispatchError` as `Pallet::Variant` via the vendored
/// metadata. [`classify_submit_failure`] matches on those names, so the
/// rendering is behaviour, not just logging.
fn describe(error: &DispatchError, metadata: &subxt::Metadata) -> String {
    let DispatchError::Module(module) = error else {
        return format!("{error:?}");
    };
    let pallet = metadata.pallet_by_error_index(module.index);
    let name = pallet.map_or("Unknown", |pallet| pallet.name());
    let variant = pallet
        .and_then(|pallet| pallet.error_variant_by_index(module.error[0]))
        .map_or_else(|| format!("Error{}", module.error[0]), |v| v.name.clone());
    format!("{name}::{variant}")
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

/// Builds the dotNS reservation extrinsic: `DotnsGateway.reserve_name`.
///
/// Optionally wrapped in `Proxy.proxy(real = attester authority, …)`.
///
/// Argument order is asserted against the connected runtime's metadata at
/// startup ([`AssetHubClient::connect`]). A chain running the older
/// proof-of-ownership variant never reaches this function.
fn build_reserve_name_tx(
    r: &Reservation,
    candidate: &[u8; 32],
    proxy_for: Option<&[u8; 32]>,
) -> DynamicPayload<Vec<Value>> {
    match proxy_for {
        Some(real) => {
            let args = vec![
                Value::unnamed_variant("Id", [Value::from_bytes(real)]),
                Value::unnamed_variant("None", []),
                Value::unnamed_variant(
                    "DotnsGateway",
                    [Value::unnamed_variant(
                        "reserve_name",
                        reserve_name_args(r, candidate),
                    )],
                ),
            ];
            dynamic::tx("Proxy", "proxy", args)
        }
        None => dynamic::tx(
            "DotnsGateway",
            "reserve_name",
            reserve_name_args(r, candidate),
        ),
    }
}

fn reserve_name_args(r: &Reservation, candidate: &[u8; 32]) -> Vec<Value> {
    let reserved_base_label = match &r.reserved_username {
        Some(name) => Value::unnamed_variant("Some", [Value::from_bytes(name.as_bytes())]),
        None => Value::unnamed_variant("None", []),
    };
    vec![
        Value::from_bytes(candidate),
        sr25519_signature(r.dotns_signature.as_deref().unwrap_or_default()),
        Value::from_bytes(r.full_username.as_bytes()),
        Value::from_bytes(&r.identifier_key),
        reserved_base_label,
        Value::u128(u128::from(
            r.dotns_signed_at.unwrap_or_default().unsigned_abs(),
        )),
    ]
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
    use chain_types::people::runtime_types::sp_runtime::ModuleError;

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
            dotns_signature: None,
            dotns_signed_at: None,
            dotns_attempt: 0,
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

    /// Every env var `from_env` reads, cleared around each case so neither a
    /// previous case nor the developer's ambient shell leaks into the next.
    const FROM_ENV_VARS: &[&str] = &[
        "DEVICE_ATTESTATION_DATABASE_URL",
        "PEOPLE_RPC_URL",
        "CHAIN_WRITER_SIGNER_SURI",
        "ATTESTER_ACCOUNT",
        "CHAIN_WRITER_HOLDER_ID",
        "CHAIN_WRITER_LEASE_NAME",
        "CHAIN_WRITER_LEASE_TTL_SECS",
        "CHAIN_WRITER_POLL_SECS",
        "CHAIN_WRITER_BATCH_SIZE",
        "CHAIN_WRITER_FINALIZE_SECS",
        "CHAIN_WRITER_MAX_ATTEMPTS",
        "QUEUE_ENABLED",
        "QUEUE_FALLBACK_AFTER_SECS",
        "PAYMENT_POLL_INTERVAL_SECS",
        "ATTESTER_RESOURCE_POLL_SECS",
        "ATTESTER_ALLOWANCE_FLOOR",
        "ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK",
    ];

    /// The vars `from_env` refuses to default.
    const REQUIRED_ENV: &[(&str, &str)] = &[
        (
            "DEVICE_ATTESTATION_DATABASE_URL",
            "postgres://writer:pw@localhost/device_attestation",
        ),
        ("CHAIN_WRITER_SIGNER_SURI", "//Writer"),
        ("ATTESTER_ACCOUNT", ALICE_SS58),
    ];

    /// Alice's well-known dev account: the SS58 form and the sr25519 public
    /// key bytes it encodes, so the parse is asserted against an independent
    /// constant rather than round-tripped through `parse_account` itself.
    const ALICE_SS58: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
    const ALICE_HEX: &str = "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";

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
    /// — never split them into separate `#[test]` fns — and the test holds
    /// [`crate::ENV_MUTEX`] because `config::tests` mutates the same
    /// process-global vars (`DEVICE_ATTESTATION_DATABASE_URL`,
    /// `PEOPLE_RPC_URL`, ...).
    /// (`queue::tests` uses its own `QUEUE_TEST_STRICT_*` names.)
    #[test]
    fn from_env_reads_and_validates_the_environment() {
        let _guard = crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Fully populated: every var lands in its field.
        let config = from_env_with(&[
            (
                "DEVICE_ATTESTATION_DATABASE_URL",
                "postgres://writer:pw@localhost/device_attestation",
            ),
            ("PEOPLE_RPC_URL", "wss://people.example"),
            ("CHAIN_WRITER_SIGNER_SURI", "//Writer"),
            ("ATTESTER_ACCOUNT", ALICE_SS58),
            ("CHAIN_WRITER_HOLDER_ID", "writer-test-1"),
            ("CHAIN_WRITER_LEASE_NAME", "custom-lease"),
            ("CHAIN_WRITER_LEASE_TTL_SECS", "45"),
            ("CHAIN_WRITER_POLL_SECS", "3"),
            ("CHAIN_WRITER_BATCH_SIZE", "50"),
            ("CHAIN_WRITER_FINALIZE_SECS", "90"),
            ("CHAIN_WRITER_MAX_ATTEMPTS", "4"),
            ("QUEUE_ENABLED", "yes"),
            ("QUEUE_FALLBACK_AFTER_SECS", " 90 "), // strict parse trims padding
            ("PAYMENT_POLL_INTERVAL_SECS", "15"),
            ("ATTESTER_RESOURCE_POLL_SECS", "120"),
            ("ATTESTER_ALLOWANCE_FLOOR", "250"),
            ("ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK", "123456789012"),
        ])
        .unwrap();
        assert_eq!(
            config.database_url.expose_secret(),
            "postgres://writer:pw@localhost/device_attestation"
        );
        assert_eq!(config.people_rpc_url, "wss://people.example");
        assert_eq!(config.signer_suri.expose_secret(), "//Writer");
        let alice: [u8; 32] = hex::decode(ALICE_HEX).unwrap().try_into().unwrap();
        assert_eq!(config.attester, alice);
        assert_eq!(config.holder_id, "writer-test-1");
        assert_eq!(config.lease_name, "custom-lease");
        assert_eq!(config.lease_ttl, Duration::from_secs(45));
        assert_eq!(config.poll_interval, Duration::from_secs(3));
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.finalize_timeout, Duration::from_secs(90));
        assert_eq!(config.max_attempts, 4);
        assert!(config.queue_enabled);
        assert_eq!(config.queue_fallback_after, Duration::from_secs(90));
        assert_eq!(config.payment_poll_interval, Duration::from_secs(15));
        assert_eq!(config.resource_poll_interval, Duration::from_secs(120));
        assert_eq!(config.allowance_floor, 250);
        assert_eq!(config.signer_balance_floor_planck, 123_456_789_012);

        // Required-only: every optional var falls back to its documented default.
        let config = from_env_with(REQUIRED_ENV).unwrap();
        assert_eq!(
            config.people_rpc_url,
            "wss://previewnet.substrate.dev/people"
        );
        // The holder id must be unique per boot (pid + random suffix), never a
        // fixed literal that would let overlapping instances share a lease.
        assert!(
            config
                .holder_id
                .starts_with(&format!("writer-{}-", std::process::id())),
            "unexpected default holder id {:?}",
            config.holder_id
        );
        assert_eq!(config.lease_name, "people-chain-writer");
        assert_eq!(config.lease_ttl, Duration::from_secs(30));
        assert_eq!(config.poll_interval, Duration::from_secs(2));
        assert_eq!(config.batch_size, 25);
        assert_eq!(config.finalize_timeout, Duration::from_secs(120));
        assert_eq!(config.max_attempts, 8);
        assert!(!config.queue_enabled);
        assert_eq!(config.queue_fallback_after, Duration::from_secs(60));
        assert_eq!(config.payment_poll_interval, Duration::from_secs(30));
        assert_eq!(config.resource_poll_interval, Duration::from_secs(60));
        assert_eq!(config.allowance_floor, 100);
        assert_eq!(config.signer_balance_floor_planck, 10_000_000_000);

        // Whitespace-only HOLDER_ID counts as unset, not as a value.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("CHAIN_WRITER_HOLDER_ID", "   "));
        let config = from_env_with(&vars).unwrap();
        assert!(config.holder_id.starts_with("writer-"));

        // Missing required vars fail naming the var.
        let err = from_env_with(&[("CHAIN_WRITER_SIGNER_SURI", "//Writer")]).unwrap_err();
        assert!(
            err.to_string().contains("DEVICE_ATTESTATION_DATABASE_URL"),
            "{err}"
        );
        // Given a database URL, the next missing required var is the one
        // reported.
        let err =
            from_env_with(&[("DEVICE_ATTESTATION_DATABASE_URL", "postgres://x")]).unwrap_err();
        assert!(
            err.to_string()
                .contains("CHAIN_WRITER_SIGNER_SURI is required"),
            "{err}"
        );

        // An unparseable ATTESTER_ACCOUNT is a startup error, and the message
        // names the var so an operator knows which one to fix.
        let mut vars = vec![
            (
                "DEVICE_ATTESTATION_DATABASE_URL",
                "postgres://writer:pw@localhost/device_attestation",
            ),
            ("CHAIN_WRITER_SIGNER_SURI", "//Writer"),
        ];
        vars.push(("ATTESTER_ACCOUNT", "not-an-account"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(err.to_string().contains("ATTESTER_ACCOUNT"), "{err}");

        // QUEUE_ENABLED only accepts boolean tokens, and the error names it.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("QUEUE_ENABLED", "maybe"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(err.to_string().contains("QUEUE_ENABLED"), "{err}");

        // The strict-u64 knobs reject garbage, naming the offending var.
        for key in [
            "QUEUE_FALLBACK_AFTER_SECS",
            "PAYMENT_POLL_INTERVAL_SECS",
            "ATTESTER_RESOURCE_POLL_SECS",
            "ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK",
        ] {
            let mut vars = REQUIRED_ENV.to_vec();
            vars.push((key, "30s"));
            let err = from_env_with(&vars).unwrap_err();
            assert!(err.to_string().contains(key), "{key}: {err}");
        }

        // ATTESTER_ALLOWANCE_FLOOR parses strictly AND must fit a u32.
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("ATTESTER_ALLOWANCE_FLOOR", "4294967296"));
        let err = from_env_with(&vars).unwrap_err();
        assert!(
            err.to_string()
                .contains("ATTESTER_ALLOWANCE_FLOOR must fit a u32"),
            "{err}"
        );

        // Known-lenient: the plain env_u64 knobs silently fall back to their
        // defaults on garbage instead of erroring (unlike the strict ones).
        let mut vars = REQUIRED_ENV.to_vec();
        vars.push(("CHAIN_WRITER_LEASE_TTL_SECS", "garbage"));
        vars.push(("CHAIN_WRITER_BATCH_SIZE", "lots"));
        let config = from_env_with(&vars).unwrap();
        assert_eq!(config.lease_ttl, Duration::from_secs(30));
        assert_eq!(config.batch_size, 25);
    }

    /// Regression: the 2026-08-05 silent-failure incident.
    #[test]
    fn module_errors_resolve_to_pallet_and_variant_names() {
        let invalid_signature = DispatchError::Module(ModuleError {
            index: 62,
            error: [1, 0, 0, 0],
        });

        assert_eq!(
            describe(&invalid_signature, chain_types::metadata()),
            "PeopleLite::InvalidAttestationSignature"
        );
        assert_eq!(
            describe(
                &DispatchError::Module(ModuleError {
                    index: 62,
                    error: [3, 0, 0, 0],
                }),
                chain_types::metadata()
            ),
            "PeopleLite::AlreadyRegistered"
        );
        assert_eq!(
            describe(&DispatchError::BadOrigin, chain_types::metadata()),
            "BadOrigin"
        );
    }

    /// The rendered name must still hit the `AlreadyRegistered` branch.
    #[test]
    fn rendered_already_registered_still_assigns() {
        assert_eq!(
            classify_submit_failure(
                "proxied call failed: PeopleLite::AlreadyRegistered",
                None,
                [7; 32],
                1,
                3
            ),
            SubmitFailureAction::Assign
        );
    }

    /// The dev default (attester == signer == `//Alice`) depends on this.
    #[test]
    fn direct_mode_submits_attest_unwrapped() {
        let reservation = reservation();
        let candidate = [7; 32];
        let payload = build_registration_tx(&reservation, &candidate, None);

        assert_eq!(payload.pallet_name(), "PeopleLite");
        assert_eq!(payload.call_name(), "attest");
    }
    // --- dotNS gateway lane -------------------------------------------------

    /// The window both Asset Hubs configure (`MaxValiditySeconds`, 3 days).
    const WINDOW: u64 = 259_200;
    /// `MaxFutureSkewSeconds` on both Asset Hubs.
    const SKEW: u64 = 30;
    const SIGNED_AT: i64 = 1_750_000_000;

    /// The freshness bounds the gate tests run against, as read from chain.
    const BOUNDS: ValidityWindow = ValidityWindow {
        max_validity_secs: WINDOW,
        max_future_skew_secs: SKEW,
    };

    /// A reservation whose stored dotns signature actually verifies. The gate
    /// tests therefore exercise the real verifier rather than a stub.
    fn signed_reservation() -> (Reservation, [u8; 32], [u8; 32]) {
        let keypair = subxt_signer::sr25519::Keypair::from_uri(
            &subxt_signer::SecretUri::from_str("//dotns-writer-test").expect("valid uri"),
        )
        .expect("keypair");
        let candidate = keypair.public_key().0;
        let attester = [11u8; 32];
        let identifier_key = vec![5; 65];

        let message = dotns::reservation_message(
            &candidate,
            &attester,
            b"testing",
            &identifier_key,
            None,
            SIGNED_AT as u64,
        );

        let mut r = reservation();
        r.identifier_key = identifier_key;
        r.dotns_signature = Some(keypair.sign(&message).0.to_vec());
        r.dotns_signed_at = Some(SIGNED_AT);
        (r, candidate, attester)
    }

    /// A well-formed row inside its validity window passes every offline gate.
    #[test]
    fn a_fresh_verified_reservation_passes_the_gates() {
        let (r, candidate, attester) = signed_reservation();
        assert_eq!(
            check_dotns_submittable(&r, &candidate, &attester, BOUNDS, SIGNED_AT + 60),
            Ok(())
        );
    }

    /// A failed Asset Hub dial parks the lane instead of stopping the writer,
    /// backs off rather than re-dialling every pass, and logs once per distinct
    /// cause rather than once per attempt.
    #[test]
    fn a_parked_dotns_lane_backs_off_and_logs_each_cause_once() {
        let t0 = Instant::now();
        let mut lane = DotnsLane::Enabled {
            rpc_url: "wss://example.invalid".to_string(),
            connected: None,
            last_error: None,
            retry_at: None,
        };

        // A lane that has never dialled dials immediately.
        assert_eq!(lane.dial_state(t0), DotnsDial::Dial);

        // First failure: worth logging, and no re-dial until the interval is up.
        assert!(lane.record_dial_failure("unreachable".to_string(), t0));
        assert_eq!(lane.dial_state(t0), DotnsDial::Skip);
        assert_eq!(
            lane.dial_state(t0 + DOTNS_RECONNECT_INTERVAL - Duration::from_secs(1)),
            DotnsDial::Skip
        );
        let t1 = t0 + DOTNS_RECONNECT_INTERVAL;
        assert_eq!(lane.dial_state(t1), DotnsDial::Dial);

        // Same cause again: silent, but still backs off. This is the shape
        // assertion case — permanent, and it must not print forever.
        assert!(!lane.record_dial_failure("unreachable".to_string(), t1));
        assert_eq!(lane.dial_state(t1), DotnsDial::Skip);

        // A different cause is worth a line.
        let t2 = t1 + DOTNS_RECONNECT_INTERVAL;
        assert!(lane.record_dial_failure("reserve_name shape mismatch".to_string(), t2));
        // ...and the original cause is new again once it has been superseded.
        let t3 = t2 + DOTNS_RECONNECT_INTERVAL;
        assert!(lane.record_dial_failure("unreachable".to_string(), t3));

        // A disabled lane never dials, whatever else happens.
        assert_eq!(DotnsLane::Disabled.dial_state(t0), DotnsDial::Skip);
        assert!(!DotnsLane::Disabled.record_dial_failure("ignored".to_string(), t0));
    }

    /// The gateway bounds `signed_at` on **both** sides, and the writer
    /// enforces the chain's own `MaxFutureSkewSeconds` rather than trusting
    /// device-attestation-api's `DOTNS_MAX_FUTURE_SKEW_SECS` to match it. Unlike every
    /// other reject this one is not terminal: the row becomes submittable as
    /// the clock advances, so it reports when.
    #[test]
    fn a_future_dated_reservation_is_deferred_not_failed() {
        let (r, candidate, attester) = signed_reservation();

        // Inside the tolerance: submittable even though the chain's clock has
        // not reached signed_at.
        assert_eq!(
            check_dotns_submittable(&r, &candidate, &attester, BOUNDS, SIGNED_AT - SKEW as i64),
            Ok(())
        );

        // One second beyond it. Deferred until the skew bound lets it through,
        // not failed — nothing about the row is wrong.
        assert_eq!(
            check_dotns_submittable(
                &r,
                &candidate,
                &attester,
                BOUNDS,
                SIGNED_AT - SKEW as i64 - 1
            ),
            Err(DotnsReject::FutureDated {
                signed_at: SIGNED_AT,
                submittable_at: SIGNED_AT - SKEW as i64,
            })
        );

        // The skew gate runs before the signature check, so a future-dated row
        // is deferred on its timestamp rather than reported as a bad signature
        // the client would try to re-sign.
        assert_eq!(
            check_dotns_submittable(
                &r,
                &candidate,
                &[99; 32],
                BOUNDS,
                SIGNED_AT - SKEW as i64 - 1
            ),
            Err(DotnsReject::FutureDated {
                signed_at: SIGNED_AT,
                submittable_at: SIGNED_AT - SKEW as i64,
            })
        );

        // A chain reporting an absurd skew bound must not overflow the
        // comparison into rejecting everything.
        let absurd = ValidityWindow {
            max_validity_secs: WINDOW,
            max_future_skew_secs: u64::MAX,
        };
        assert_eq!(
            check_dotns_submittable(&r, &candidate, &attester, absurd, SIGNED_AT),
            Ok(())
        );
    }

    /// Each gate produces its own terminal state. `EXPIRED` is separate from
    /// `FAILED_TERMINAL`. The client can act on it by re-signing.
    #[test]
    fn each_offline_gate_maps_to_its_own_terminal_state() {
        let (r, candidate, attester) = signed_reservation();

        // Aged out. Reported as expired even one second past the bound.
        assert_eq!(
            check_dotns_submittable(
                &r,
                &candidate,
                &attester,
                BOUNDS,
                SIGNED_AT + WINDOW as i64 + 1
            ),
            Err(DotnsReject::Expired {
                signed_at: SIGNED_AT,
                deadline_secs: WINDOW
            })
        );
        // Still valid on the final second of the window.
        assert_eq!(
            check_dotns_submittable(&r, &candidate, &attester, BOUNDS, SIGNED_AT + WINDOW as i64),
            Ok(())
        );

        // A different attester than the one the client bound its signature to.
        assert_eq!(
            check_dotns_submittable(&r, &candidate, &[99; 32], BOUNDS, SIGNED_AT),
            Err(DotnsReject::BadSignature)
        );

        // No dotns block at all. Not in the lane.
        assert_eq!(
            check_dotns_submittable(&reservation(), &candidate, &attester, BOUNDS, SIGNED_AT),
            Err(DotnsReject::NotInLane)
        );

        // A label with no digit suffix cannot be a valid lite label.
        let mut no_digits = r.clone();
        no_digits.full_username = "testing".to_string();
        assert!(matches!(
            check_dotns_submittable(&no_digits, &candidate, &attester, BOUNDS, SIGNED_AT),
            Err(DotnsReject::UnbuildableLabel(_))
        ));

        // BaseLabel is BoundedVec<u8, 32> on both ends of the call.
        let mut long_label = r.clone();
        long_label.full_username = format!("{}.42", "a".repeat(30));
        assert!(matches!(
            check_dotns_submittable(&long_label, &candidate, &attester, BOUNDS, SIGNED_AT),
            Err(DotnsReject::UnbuildableLabel(_))
        ));

        // reservedUsername is unbounded at intake today. The writer catches it
        // rather than failing to build the extrinsic.
        let mut long_reserved = r.clone();
        long_reserved.reserved_username = Some("a".repeat(33));
        assert!(matches!(
            check_dotns_submittable(&long_reserved, &candidate, &attester, BOUNDS, SIGNED_AT),
            Err(DotnsReject::UnbuildableReserved(_))
        ));
    }

    /// An expired row is reported as expired even when its signature is also
    /// wrong. Expiry is the state the client can recover from.
    #[test]
    fn expiry_is_reported_ahead_of_a_bad_signature() {
        let (mut r, candidate, attester) = signed_reservation();
        r.dotns_signature = Some(vec![0; 64]);
        assert!(matches!(
            check_dotns_submittable(
                &r,
                &candidate,
                &attester,
                BOUNDS,
                SIGNED_AT + WINDOW as i64 + 1
            ),
            Err(DotnsReject::Expired { .. })
        ));
    }

    #[test]
    fn direct_reservation_targets_the_gateway_pallet() {
        let (r, candidate, _) = signed_reservation();
        let payload = build_reserve_name_tx(&r, &candidate, None);

        assert_eq!(payload.pallet_name(), "DotnsGateway");
        assert_eq!(payload.call_name(), "reserve_name");
        // One argument per `RESERVE_NAME_FIELDS` entry. Same order.
        assert_eq!(payload.call_data().len(), dotns::RESERVE_NAME_FIELDS.len());
        assert_eq!(payload.call_data(), &reserve_name_args(&r, &candidate));
    }

    #[test]
    fn proxied_reservation_wraps_reserve_name_directly() {
        let (r, candidate, _) = signed_reservation();
        let proxy_for = [8; 32];
        let payload = build_reserve_name_tx(&r, &candidate, Some(&proxy_for));

        assert_eq!(payload.pallet_name(), "Proxy");
        assert_eq!(payload.call_name(), "proxy");
        assert_eq!(
            payload.call_data()[2],
            Value::unnamed_variant(
                "DotnsGateway",
                [Value::unnamed_variant(
                    "reserve_name",
                    reserve_name_args(&r, &candidate)
                )]
            )
        );
    }
}
