//! One-shot bootstrap of the local projection from one finalized chain snapshot.

use chain_types::people;
use chain_types::people::runtime_types::indiv_pallet_resources::types::ConsumerInfo;
use sqlx::PgPool;
use subxt::utils::AccountId32;

use crate::chain::{BoxError, ChainClient, ChainError};
use crate::projection::{self, AssignedUsername, PROJECTION_LOCK_ID};
use crate::ss58::Ss58Error;

/// Result of one complete finalized bootstrap scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapReport {
    /// Number of records persisted.
    pub indexed: u64,
    /// Number of malformed iterator entries skipped.
    pub skipped: u64,
    /// Finalized block number used for the whole scan.
    pub snapshot_number: u64,
}

/// Fatal bootstrap failure.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// A chain snapshot operation failed.
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// Subxt rejected a storage or constant operation.
    #[error("reading finalized People Chain storage: {0}")]
    Storage(#[source] BoxError),
    /// The chain's SS58 prefix could not encode account identifiers.
    #[error(transparent)]
    Ss58(#[from] Ss58Error),
    /// A finalized block number could not fit the database column.
    #[error("finalized block number {0} exceeds the database range")]
    SnapshotNumber(u64),
    /// A projection write failed.
    #[error("writing username projection: {0}")]
    Database(#[from] sqlx::Error),
    /// The storage iterator failed before row-level decoding completed.
    #[error("finalized storage scan was incomplete after {iterator_errors} iterator errors")]
    IncompleteScan {
        /// Number of iterator errors observed.
        iterator_errors: u64,
    },
}

/// Seed the projection from a full finalized snapshot **only if** the checkpoint
/// is empty (a fresh database); otherwise leave the existing projection in place
/// for incremental catch-up. Returns `Some(report)` when a bootstrap ran, `None`
/// when an existing checkpoint was found.
///
/// Serialized on [`PROJECTION_LOCK_ID`] (a blocking Postgres advisory lock held
/// for the whole check-and-seed) so concurrent replicas can neither both
/// bootstrap nor bootstrap while another instance is indexing. Restarts with a
/// populated checkpoint skip the full re-scan entirely.
pub async fn ensure_seeded(
    pool: &PgPool,
    chain: &ChainClient,
    page_size: u32,
) -> Result<Option<BootstrapReport>, BootstrapError> {
    let mut lock_connection = pool.acquire().await?;
    lock_connection.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(PROJECTION_LOCK_ID)
        .execute(&mut *lock_connection)
        .await?;

    let already_seeded = sqlx::query("SELECT 1 FROM sync_state WHERE id = 1")
        .fetch_optional(pool)
        .await?
        .is_some();
    if already_seeded {
        return Ok(None);
    }

    bootstrap_locked(pool, chain, page_size).await.map(Some)
}

async fn bootstrap_locked(
    pool: &PgPool,
    chain: &ChainClient,
    page_size: u32,
) -> Result<BootstrapReport, BootstrapError> {
    let at = chain
        .online()
        .at_current_block()
        .await
        .map_err(|source| ChainError::Query(Box::new(source)))?;
    let snapshot_hash = at.block_hash().0;
    let snapshot_number = at.block_number();
    let snapshot_number_db = i64::try_from(snapshot_number)
        .map_err(|_| BootstrapError::SnapshotNumber(snapshot_number))?;
    let ss58_prefix = at
        .constants()
        .entry(people::constants().system().ss58_prefix())
        .map_err(|source| BootstrapError::Storage(Box::new(source)))?;
    if ss58_prefix >= 16_384 {
        return Err(Ss58Error::InvalidPrefix(ss58_prefix).into());
    }

    let address = people::storage().resources().consumers();
    let mut entries = at
        .storage()
        .entry(address)
        .map_err(|source| BootstrapError::Storage(Box::new(source)))?
        .iter(())
        .await
        .map_err(|source| BootstrapError::Storage(Box::new(source)))?;
    let mut batch = Vec::with_capacity(page_size as usize);
    let mut indexed = 0_u64;
    let mut skipped = 0_u64;
    let mut iterator_errors = 0_u64;

    while let Some(entry) = entries.next().await {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                skipped += 1;
                iterator_errors += 1;
                tracing::warn!(stage = "iterator", error = ?error, "skipping malformed consumer");
                continue;
            }
        };
        let (account_id,): (AccountId32,) = match entry.key().and_then(|key| key.decode()) {
            Ok(key) => key,
            Err(error) => {
                skipped += 1;
                tracing::warn!(stage = "key", error = ?error, "skipping malformed consumer");
                continue;
            }
        };
        let consumer: ConsumerInfo = match entry.value().decode() {
            Ok(value) => value,
            Err(error) => {
                skipped += 1;
                tracing::warn!(stage = "value", account_id = ?account_id, error = ?error, "skipping malformed consumer");
                continue;
            }
        };

        match projection::decode_consumer(
            account_id.0,
            consumer.identifier_key,
            consumer.lite_username.0,
            consumer.full_username.map(|username| username.0),
            ss58_prefix,
            snapshot_hash,
            snapshot_number,
        ) {
            Ok(record) => batch.push(record),
            Err(error) => {
                skipped += 1;
                tracing::warn!(stage = "username", account_id = ?account_id, error = ?error, "skipping malformed consumer");
            }
        }

        if batch.len() == page_size as usize {
            upsert_batch(pool, &batch, snapshot_number_db).await?;
            indexed += batch.len() as u64;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        upsert_batch(pool, &batch, snapshot_number_db).await?;
        indexed += batch.len() as u64;
    }

    if iterator_errors != 0 {
        return Err(BootstrapError::IncompleteScan { iterator_errors });
    }

    sqlx::query("DELETE FROM assigned_usernames WHERE snapshot_hash <> $1")
        .bind(snapshot_hash.as_slice())
        .execute(pool)
        .await?;

    write_checkpoint(pool, snapshot_number_db, snapshot_hash, indexed, skipped).await?;

    Ok(BootstrapReport {
        indexed,
        skipped,
        snapshot_number,
    })
}

/// Persist the resumable finalized checkpoint after a full snapshot commits.
///
/// Written last in the success path — only after the reconciliation delete
/// succeeds — so the checkpoint advances solely when the whole snapshot landed.
/// Idempotent: re-running the same finalized range overwrites the single row.
async fn write_checkpoint(
    pool: &PgPool,
    snapshot_number: i64,
    snapshot_hash: [u8; 32],
    indexed: u64,
    skipped: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO sync_state (
            id, last_finalized_number, last_finalized_hash, last_synced_at,
            records_indexed, decode_failures, updated_at
         ) VALUES (1, $1, $2, now(), $3, $4, now())
         ON CONFLICT (id) DO UPDATE SET
            last_finalized_number = EXCLUDED.last_finalized_number,
            last_finalized_hash = EXCLUDED.last_finalized_hash,
            last_synced_at = EXCLUDED.last_synced_at,
            records_indexed = EXCLUDED.records_indexed,
            decode_failures = EXCLUDED.decode_failures,
            updated_at = now()",
    )
    .bind(snapshot_number)
    .bind(snapshot_hash.as_slice())
    .bind(indexed as i64)
    .bind(skipped as i64)
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_batch(
    pool: &PgPool,
    records: &[AssignedUsername],
    snapshot_number: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for record in records {
        projection::upsert(&mut tx, record, snapshot_number).await?;
    }
    tx.commit().await
}
