//! Single-instance leases over the `writer_lease` table.
//!
//! Shared by the two single-instance daemons in the identity boundary — the
//! `identity-chain-writer` (name `people-chain-writer`) and the
//! `registration-queue` advancer (name [`crate::queue::ADVANCER_LEASE_NAME`]).
//! A lease is a best-effort deploy-overlap guard, not a fence; it doubles as a
//! **liveness signal**: the writer reads the advancer's lease freshness for
//! its stranded-queue warning (queue enabled) and its janitor drain of
//! leftover `QUEUED` rows (queue disabled) — see `crate::queue`.

use std::time::Duration;

use sqlx::{PgPool, Row as _};

/// Take (or retake) the named lease: free, expired, or already ours. Returns
/// the new epoch, or `None` while another live holder has it.
pub async fn try_acquire(
    pool: &PgPool,
    name: &str,
    holder_id: &str,
    ttl: Duration,
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
    .bind(ttl.as_secs() as i64)
    .fetch_optional(pool)
    .await?;
    row.map(|row| row.try_get("lease_epoch")).transpose()
}

/// Renew the lease. Returns `false` if it is no longer ours — including when
/// it has already **expired**: an expired lease may have been observed as dead
/// by the liveness readers (the stranded-queue warning, the janitor drain),
/// so the holder must go back through [`try_acquire`] (a new epoch) rather
/// than silently resurrecting it.
pub async fn renew(
    pool: &PgPool,
    name: &str,
    holder_id: &str,
    epoch: i64,
    ttl: Duration,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE writer_lease \
         SET expires_at = now() + ($4 * interval '1 second'), heartbeat_at = now() \
         WHERE name = $1 AND holder_id = $2 AND lease_epoch = $3 AND expires_at > now()",
    )
    .bind(name)
    .bind(holder_id)
    .bind(epoch)
    .bind(ttl.as_secs() as i64)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Whether the named lease is currently held and unexpired — the liveness
/// probe behind the stranded-queue signal.
pub async fn alive(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    let row =
        sqlx::query("SELECT 1 AS held FROM writer_lease WHERE name = $1 AND expires_at > now()")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}
