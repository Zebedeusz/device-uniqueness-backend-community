//! Incremental finalized-block, event-signalled `Resources::Consumers` indexer.
//!
//! After the one-shot [`crate::bootstrap`] seeds the projection and the
//! `sync_state` checkpoint, this module drives the steady-state resync loop. For
//! each finalized block above the checkpoint it decodes the `Resources`
//! registration/mutation events (which carry only an account, never a username)
//! to learn which accounts changed, re-reads `Resources::Consumers(account)` at
//! that same finalized block for the authoritative value, and upserts (or
//! deletes when the entry is absent) each affected row inside one transaction
//! that also advances the per-block checkpoint. Cost therefore scales with the
//! number of changed accounts per block, not the total size of `Consumers`.
//!
//! # Reorg-safety
//!
//! Blocks are only ever processed by number up to the finalized head observed
//! via [`subxt`]'s `at_current_block`, so no reorg-sensitive state is indexed.
//!
//! # Idempotency
//!
//! Reprocessing a block re-reads authoritative storage and upserts by the
//! `account_id` primary key, so repeats are value-equivalent (only `updated_at`
//! changes). Because the per-block checkpoint advances inside the same
//! transaction as its writes, an interrupted block never advances the
//! checkpoint or leaves partial state.
//!
//! # `sync_state` semantics in incremental mode
//!
//! In incremental mode `sync_state.last_finalized_*` is the per-block checkpoint
//! (the last fully committed block), and each `assigned_usernames.snapshot_hash`
//! becomes "the last block that touched this row" rather than a single shared
//! snapshot. That divergence from the [`crate::bootstrap`] single-snapshot value
//! is expected and harmless: incremental never runs the bootstrap reconciliation
//! delete (`DELETE ... WHERE snapshot_hash <> $1`), which is the only code that
//! relies on every row sharing one snapshot hash.

use std::collections::BTreeSet;

use chain_types::people;
use chain_types::people::runtime_types::indiv_pallet_resources::types::ConsumerInfo;
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use subxt::utils::AccountId32;

use crate::chain::{BoxError, ChainClient, ChainError};
use crate::projection;
use crate::ss58::Ss58Error;

/// Outcome of one [`index_finalized_range`] pass across finalized blocks.
#[derive(Debug, Clone, Copy)]
pub struct IndexReport {
    /// First finalized block number considered (checkpoint + 1).
    pub from_block: u64,
    /// Finalized head block number reached.
    pub to_block: u64,
    /// Number of finalized blocks processed in this pass.
    pub blocks_processed: u64,
    /// Number of accounts upserted from re-read consumer storage.
    pub accounts_upserted: u64,
    /// Number of accounts deleted because their consumer entry was absent.
    pub accounts_deleted: u64,
    /// Number of consumer values that failed username decoding and were skipped.
    pub decode_failures: u64,
}

/// Fatal incremental indexing failure.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// A finalized block or event query failed.
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// Subxt rejected a storage or constant operation.
    #[error("reading finalized People Chain storage: {0}")]
    Storage(#[source] BoxError),
    /// The chain's SS58 prefix could not encode account identifiers.
    #[error(transparent)]
    Ss58(#[from] Ss58Error),
    /// A projection write failed.
    #[error("writing username projection: {0}")]
    Database(#[from] sqlx::Error),
    /// A finalized block number could not fit the database column.
    #[error("finalized block number {0} exceeds the database range")]
    SnapshotNumber(u64),
}

/// Index every finalized block from the stored checkpoint up to the head.
///
/// Acquires the shared projection advisory lock with `pg_try_advisory_lock`;
/// if another instance already holds it, returns `Ok(None)` so this pass is
/// skipped (only one writer mutates the projection at a time). When held, reads
/// the resumable checkpoint from `sync_state`, then processes each finalized
/// block above it, committing per-block so an interrupted pass never advances
/// past the last fully-written block. Returns `Ok(Some(_))` with a no-op report
/// when no new finalized block exists, or a zero report when the checkpoint row
/// is missing (bootstrap seeds it at startup).
pub async fn index_finalized_range(
    pool: &PgPool,
    chain: &ChainClient,
) -> Result<Option<IndexReport>, IndexError> {
    let mut lock_connection = pool.acquire().await?;
    lock_connection.close_on_drop();
    let acquired: bool = sqlx::query("SELECT pg_try_advisory_lock($1)")
        .bind(crate::projection::PROJECTION_LOCK_ID)
        .fetch_one(&mut *lock_connection)
        .await?
        .try_get(0)?;
    if !acquired {
        return Ok(None);
    }

    let checkpoint_row = sqlx::query("SELECT last_finalized_number FROM sync_state WHERE id = 1")
        .fetch_optional(pool)
        .await?;
    let Some(checkpoint_row) = checkpoint_row else {
        return Ok(Some(IndexReport {
            from_block: 0,
            to_block: 0,
            blocks_processed: 0,
            accounts_upserted: 0,
            accounts_deleted: 0,
            decode_failures: 0,
        }));
    };
    let checkpoint = checkpoint_row.try_get::<i64, _>("last_finalized_number")?;
    let checkpoint = u64::try_from(checkpoint).unwrap_or(0);

    let head = chain
        .online()
        .at_current_block()
        .await
        .map_err(|source| ChainError::Query(Box::new(source)))?;
    let head_number = head.block_number();
    if head_number <= checkpoint {
        return Ok(Some(IndexReport {
            from_block: checkpoint,
            to_block: checkpoint,
            blocks_processed: 0,
            accounts_upserted: 0,
            accounts_deleted: 0,
            decode_failures: 0,
        }));
    }

    let mut report = IndexReport {
        from_block: checkpoint + 1,
        to_block: head_number,
        blocks_processed: 0,
        accounts_upserted: 0,
        accounts_deleted: 0,
        decode_failures: 0,
    };

    for number in (checkpoint + 1)..=head_number {
        index_block(pool, chain, number, &mut report).await?;
        report.blocks_processed += 1;
    }

    Ok(Some(report))
}

/// Index one finalized block, committing its writes and checkpoint atomically.
async fn index_block(
    pool: &PgPool,
    chain: &ChainClient,
    number: u64,
    report: &mut IndexReport,
) -> Result<(), IndexError> {
    let at = chain
        .online()
        .at_block(number)
        .await
        .map_err(|source| ChainError::Query(Box::new(source)))?;
    let block_hash = at.block_hash().0;
    let block_number = at.block_number();
    let block_number_db =
        i64::try_from(block_number).map_err(|_| IndexError::SnapshotNumber(block_number))?;

    let events = at
        .events()
        .fetch()
        .await
        .map_err(|source| ChainError::Query(Box::new(source)))?;

    let mut affected: Vec<[u8; 32]> = Vec::new();
    for event in events.find::<people::resources::events::LitePersonRegistered>() {
        let event = event.map_err(|source| IndexError::Storage(Box::new(source)))?;
        affected.push(event.account.0);
    }
    for event in events.find::<people::resources::events::PersonRegistered>() {
        let event = event.map_err(|source| IndexError::Storage(Box::new(source)))?;
        affected.push(event.account.0);
    }
    for event in events.find::<people::resources::events::IdentifierKeyUpdated>() {
        let event = event.map_err(|source| IndexError::Storage(Box::new(source)))?;
        affected.push(event.account.0);
    }
    for event in events.find::<people::people_lite::events::ConsumerRegistered>() {
        let event = event.map_err(|source| IndexError::Storage(Box::new(source)))?;
        affected.push(event.account.0);
    }
    let affected = dedupe_accounts(affected);

    if affected.is_empty() {
        let mut tx = pool.begin().await?;
        advance_checkpoint(&mut tx, block_number_db, &block_hash, 0, 0).await?;
        tx.commit().await?;
        return Ok(());
    }

    let ss58_prefix = crate::ss58::validate_prefix(
        at.constants()
            .entry(people::constants().system().ss58_prefix())
            .map_err(|source| IndexError::Storage(Box::new(source)))?,
    )?;

    let mut tx = pool.begin().await?;
    let mut block_upserts = 0_u64;
    let mut block_deletes = 0_u64;
    let mut block_failures = 0_u64;

    for account in &affected {
        let consumer = at
            .storage()
            .try_fetch(
                people::storage().resources().consumers(),
                (AccountId32(*account),),
            )
            .await
            .map_err(|source| IndexError::Storage(Box::new(source)))?;
        match consumer {
            Some(value) => {
                let consumer: ConsumerInfo = value
                    .decode()
                    .map_err(|source| IndexError::Storage(Box::new(source)))?;
                match projection::decode_consumer(
                    *account,
                    consumer.identifier_key,
                    consumer.lite_username.0,
                    consumer.full_username.map(|username| username.0),
                    ss58_prefix,
                    block_hash,
                    block_number,
                ) {
                    Ok(record) => {
                        projection::upsert(&mut tx, &record, block_number_db).await?;
                        block_upserts += 1;
                    }
                    Err(error) => {
                        projection::delete_account(&mut tx, account).await?;
                        block_failures += 1;
                        tracing::warn!(stage = "username", account = ?account, error = ?error, "deleting now-malformed consumer");
                    }
                }
            }
            None => {
                projection::delete_account(&mut tx, account).await?;
                block_deletes += 1;
            }
        }
    }

    advance_checkpoint(
        &mut tx,
        block_number_db,
        &block_hash,
        block_upserts,
        block_failures,
    )
    .await?;
    tx.commit().await?;

    report.accounts_upserted += block_upserts;
    report.accounts_deleted += block_deletes;
    report.decode_failures += block_failures;
    Ok(())
}

/// Advance the single-row checkpoint to `block_number` within an open tx.
async fn advance_checkpoint(
    tx: &mut Transaction<'_, Postgres>,
    block_number: i64,
    block_hash: &[u8; 32],
    records_indexed: u64,
    decode_failures: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE sync_state SET
            last_finalized_number = $1,
            last_finalized_hash = $2,
            last_synced_at = now(),
            records_indexed = $3,
            decode_failures = $4,
            updated_at = now()
         WHERE id = 1",
    )
    .bind(block_number)
    .bind(block_hash.as_slice())
    .bind(records_indexed as i64)
    .bind(decode_failures as i64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deduplicate affected accounts deterministically for stable per-block reads.
fn dedupe_accounts(accounts: impl IntoIterator<Item = [u8; 32]>) -> Vec<[u8; 32]> {
    accounts
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::dedupe_accounts;

    #[test]
    fn dedupe_removes_duplicates_and_sorts() {
        let deduped = dedupe_accounts([[3; 32], [1; 32], [3; 32], [2; 32], [1; 32]]);
        assert_eq!(deduped, vec![[1; 32], [2; 32], [3; 32]]);
    }

    #[test]
    fn dedupe_is_deterministic_regardless_of_input_order() {
        let forward = dedupe_accounts([[1; 32], [2; 32], [3; 32]]);
        let reversed = dedupe_accounts([[3; 32], [2; 32], [1; 32]]);
        assert_eq!(forward, reversed);
    }
}
