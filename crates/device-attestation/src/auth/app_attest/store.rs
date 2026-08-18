//! Persistence for registered App Attest keys (`app_attest_keys`).

use sqlx::PgPool;

/// A stored App Attest key row.
pub struct StoredKey {
    /// Uncompressed SEC1 P-256 public key (65 bytes).
    pub public_key: Vec<u8>,
    /// Last accepted assertion counter.
    pub sign_count: i64,
}

/// Insert or re-register a key. Re-registration (same `key_id`) replaces the
/// stored material and resets the counter — a fresh attestation restarts the
/// key's lifecycle.
pub async fn upsert(
    pool: &PgPool,
    key_id: &[u8],
    public_key: &[u8],
    receipt: &[u8],
    registering_client_id: Option<&[u8]>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_attest_keys (key_id, public_key, receipt, registering_client_id) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (key_id) DO UPDATE \
         SET public_key = EXCLUDED.public_key, receipt = EXCLUDED.receipt, sign_count = 0",
    )
    .bind(key_id)
    .bind(public_key)
    .bind(receipt)
    .bind(registering_client_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Load a registered key by id.
pub async fn find(pool: &PgPool, key_id: &[u8]) -> Result<Option<StoredKey>, sqlx::Error> {
    let row: Option<(Vec<u8>, i64)> =
        sqlx::query_as("SELECT public_key, sign_count FROM app_attest_keys WHERE key_id = $1")
            .bind(key_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(public_key, sign_count)| StoredKey {
        public_key,
        sign_count,
    }))
}

/// Persist the counter from an accepted assertion, only if it still moves
/// forward (concurrent assertions race between read and commit). Returns
/// whether the row advanced.
pub async fn commit_sign_count(
    pool: &PgPool,
    key_id: &[u8],
    sign_count: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE app_attest_keys SET sign_count = $2, last_asserted_at = now() \
         WHERE key_id = $1 AND sign_count < $2",
    )
    .bind(key_id)
    .bind(sign_count)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
