//! Continuous finalized resync: incremental finalized-block polling plus the
//! shared freshness holder that readiness reports.
//!
//! Each tick calls [`crate::incremental::index_finalized_range`], which advances
//! the `sync_state` checkpoint one finalized block at a time. For every block
//! above the checkpoint it decodes the `Resources` registration/mutation events
//! to find changed accounts, re-reads `Resources::Consumers(account)` at that
//! finalized block for the authoritative username, and upserts (or deletes when
//! absent) each affected row inside one transaction that also advances the
//! per-block checkpoint. Cost scales with the number of changed accounts, not
//! the total size of `Consumers`. The one-time startup [`crate::bootstrap`]
//! seeds and reconciles the projection and sets the initial checkpoint. Because
//! only numbers up to the finalized head are processed and reprocessing re-reads
//! authoritative storage keyed by account, the loop is reorg-safe and
//! idempotent: an interrupted pass never advances past the last committed block.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

use crate::chain::ChainClient;
use crate::config::Config;
use crate::incremental::index_finalized_range;

const BACKOFF_BASE_SECS: u64 = 1;
const BACKOFF_MAX_SECS: u64 = 60;

/// A point-in-time view of the projection's finalized-sync freshness.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessSnapshot {
    /// Finalized block number observed by the last successful sync.
    pub last_finalized_number: u64,
    /// Wall-clock time the last successful sync completed.
    #[serde(with = "time::serde::rfc3339")]
    pub last_synced_at: OffsetDateTime,
    /// Records persisted during the last successful sync.
    pub records_indexed: u64,
    /// Malformed records skipped during the last successful sync.
    pub decode_failures: u64,
}

/// Shared, cheaply-cloneable holder for the latest [`FreshnessSnapshot`].
///
/// The sync loop updates it after every successful snapshot; readiness reads it
/// without blocking. `None` until the first successful sync completes.
#[derive(Clone, Default)]
pub struct Freshness(Arc<RwLock<Option<FreshnessSnapshot>>>);

impl Freshness {
    /// Create an empty freshness holder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the current freshness view with a newer snapshot.
    pub fn update(&self, snapshot: FreshnessSnapshot) {
        *self.0.write().expect("freshness lock poisoned") = Some(snapshot);
    }

    /// Read the latest freshness view, or `None` before the first sync.
    pub fn snapshot(&self) -> Option<FreshnessSnapshot> {
        *self.0.read().expect("freshness lock poisoned")
    }
}

/// Load the persisted checkpoint as a freshness snapshot, if the projection has
/// already been seeded. Used at startup to seed freshness when a full bootstrap
/// is skipped because a checkpoint already exists.
pub async fn checkpoint_freshness(pool: &PgPool) -> Result<Option<FreshnessSnapshot>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT last_finalized_number, last_synced_at, records_indexed, decode_failures
         FROM sync_state WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(FreshnessSnapshot {
        last_finalized_number: u64::try_from(row.try_get::<i64, _>("last_finalized_number")?)
            .unwrap_or(0),
        last_synced_at: row.try_get("last_synced_at")?,
        records_indexed: u64::try_from(row.try_get::<i64, _>("records_indexed")?).unwrap_or(0),
        decode_failures: u64::try_from(row.try_get::<i64, _>("decode_failures")?).unwrap_or(0),
    }))
}

/// Run the incremental finalized resync loop until the task is dropped.
///
/// Ticks on the configured interval, indexes every finalized block from the
/// stored checkpoint up to the head, and on success updates `freshness` and
/// logs structured progress. On failure it logs a warning and waits a bounded
/// exponential backoff before retrying, never advancing the checkpoint. The
/// caller runs the first bootstrap at startup, so this waits one interval before
/// the first resync.
pub async fn run(pool: PgPool, chain: ChainClient, config: Config, freshness: Freshness) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.sync_interval_secs.into()));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;

    let mut consecutive_failures = 0_u32;
    loop {
        interval.tick().await;
        let started = Instant::now();
        // Before indexing, so the gauges describe the backlog this pass is
        // about to work through rather than the zero left behind it.
        if let Err(error) = record_lag_gauges(&pool, &chain).await {
            tracing::warn!(error = ?error, "checkpoint lag gauge pass failed");
        }
        match index_finalized_range(&pool, &chain).await {
            Ok(Some(report)) => {
                consecutive_failures = 0;
                freshness.update(FreshnessSnapshot {
                    last_finalized_number: report.to_block,
                    last_synced_at: OffsetDateTime::now_utc(),
                    records_indexed: report.accounts_upserted,
                    decode_failures: report.decode_failures,
                });
                tracing::info!(
                    from_block = report.from_block,
                    to_block = report.to_block,
                    blocks_processed = report.blocks_processed,
                    accounts_upserted = report.accounts_upserted,
                    accounts_deleted = report.accounts_deleted,
                    decode_failures = report.decode_failures,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "finalized resync complete"
                );
            }
            Ok(None) => {
                consecutive_failures = 0;
                tracing::debug!("another instance holds the projection lock; skipping this pass");
            }
            Err(error) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let delay = backoff(consecutive_failures);
                tracing::warn!(
                    error = ?error,
                    consecutive_failures,
                    backoff_secs = delay.as_secs(),
                    "finalized resync failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Record the finalized head, the checkpoint, and the gap between them.
///
/// The gap is the projection's real staleness — search answers from the
/// checkpoint, so it can grow while every individual pass reports success.
async fn record_lag_gauges(pool: &PgPool, chain: &ChainClient) -> anyhow::Result<()> {
    let head = chain.online().at_current_block().await?.block_number();
    metrics::gauge!("dub_chain_finalized_head_block").set(head as f64);

    let Some(snapshot) = checkpoint_freshness(pool).await? else {
        return Ok(());
    };
    let checkpoint = snapshot.last_finalized_number;
    metrics::gauge!("dub_indexer_checkpoint_block").set(checkpoint as f64);
    metrics::gauge!("dub_indexer_checkpoint_lag_blocks")
        .set(head.saturating_sub(checkpoint) as f64);
    Ok(())
}

/// Bounded exponential backoff: `BASE * 2^(failures - 1)`, capped at
/// [`BACKOFF_MAX_SECS`]. `failures` is the count of consecutive failures,
/// starting at 1 for the first.
fn backoff(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(16);
    let secs = BACKOFF_BASE_SECS
        .saturating_mul(1_u64 << shift)
        .min(BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::{backoff, Freshness, FreshnessSnapshot, BACKOFF_MAX_SECS};

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        assert_eq!(backoff(0).as_secs(), 1);
        assert_eq!(backoff(1).as_secs(), 1);
        assert_eq!(backoff(2).as_secs(), 2);
        assert_eq!(backoff(3).as_secs(), 4);
        assert_eq!(backoff(7).as_secs(), 60);
        assert_eq!(backoff(1000).as_secs(), BACKOFF_MAX_SECS);
    }

    #[test]
    fn freshness_starts_empty_and_reports_latest_update() {
        let freshness = Freshness::new();
        assert!(freshness.snapshot().is_none());

        let first = FreshnessSnapshot {
            last_finalized_number: 10,
            last_synced_at: datetime!(2026-07-11 10:00 UTC),
            records_indexed: 3,
            decode_failures: 0,
        };
        freshness.update(first);
        let stored = freshness.snapshot().expect("snapshot present");
        assert_eq!(stored.last_finalized_number, 10);
        assert_eq!(stored.records_indexed, 3);

        freshness.update(FreshnessSnapshot {
            last_finalized_number: 11,
            last_synced_at: datetime!(2026-07-11 10:01 UTC),
            records_indexed: 4,
            decode_failures: 1,
        });
        let latest = freshness.snapshot().expect("snapshot present");
        assert_eq!(latest.last_finalized_number, 11);
        assert_eq!(latest.decode_failures, 1);
    }
}
