//! One-shot puzzle consumption: the cross-replica replay guard.
//!
//! Legacy kept spent session ids in a per-process map, so a solved puzzle could
//! be replayed against a different replica. This service owns a Postgres
//! database, so the consume is a primary-key insert instead: the first writer
//! wins and every other replica sees the conflict.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Record `session_id` as spent, retained until `retain_until_ms`.
///
/// Returns `true` when this call consumed the puzzle and `false` when it was
/// already spent (the replay case).
pub async fn consume(
    pool: &PgPool,
    session_id: Uuid,
    retain_until_ms: i64,
) -> Result<bool, sqlx::Error> {
    let expires_at =
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(retain_until_ms) * 1_000_000)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

    let result = sqlx::query(
        "INSERT INTO spent_puzzles (session_id, expires_at) \
         VALUES ($1, $2) ON CONFLICT (session_id) DO NOTHING",
    )
    .bind(session_id)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Delete spent-puzzle rows whose validity window has passed.
///
/// Safe to run from any replica at any time: an expired row can never admit a
/// replay, because verification rejects the puzzle on expiry first.
pub async fn prune_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM spent_puzzles WHERE expires_at < now()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
