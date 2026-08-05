//! Reservation outbox — the durable queue + state machine behind username
//! registration.
//!
//! `POST /api/v1/usernames` inserts a [`Status::Reserved`] row and returns 202;
//! the `identity-chain-writer` binary claims due rows, submits `PeopleLite.attest`, and
//! advances them. The row is the single source of truth — the chain is
//! reconciled *to* the row, never the reverse.
//!
//! Writer mutations are guarded by the [`Guard`] lease check so a stale writer
//! that lost the lease can never advance a row. The lease is a best-effort
//! deploy-overlap guard, not a fence — the chain nonce + reconciliation is the
//! real serializer.

use std::collections::BTreeSet;

use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

/// Outbox state.
///
/// ```text
/// QUEUED ──queue advancer slot pick──▶ RESERVED
/// RESERVED ──build+sign+broadcast──▶ SUBMITTING
/// SUBMITTING ──included / AlreadyRegistered──▶ ASSIGNED     (terminal success)
/// SUBMITTING ──transient / unknown──▶ RETRY_AFTER           (reconcile, backoff)
/// SUBMITTING ──permanent error──────▶ FAILED_TERMINAL       (terminal failure)
/// RETRY_AFTER ──not_before reached──▶ (re-picked as due)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting in the registration queue (`QUEUE_ENABLED` intake). Entry state;
    /// while the queue is enabled only the advancer moves it to `RESERVED`
    /// (queue disabled: the chain-writer's janitor drains leftovers).
    Queued,
    /// Validated + persisted, not yet submitted. Entry state when the queue is
    /// disabled; otherwise reached via the advancer.
    Reserved,
    /// Extrinsic built + signed + broadcast; recorded before awaiting inclusion.
    Submitting,
    /// Confirmed on-chain (or reconciled as already-owned). Terminal success.
    Assigned,
    /// Transient failure; re-enqueued with backoff via `not_before`.
    RetryAfter,
    /// Permanent failure (bad input, unrecoverable dispatch). Terminal.
    FailedTerminal,
}

impl Status {
    /// Every status, so a gauge pass can report `0` for the ones no row is in
    /// (a metric that vanishes when the state empties is unalertable).
    pub const ALL: [Status; 6] = [
        Status::Queued,
        Status::Reserved,
        Status::Submitting,
        Status::Assigned,
        Status::RetryAfter,
        Status::FailedTerminal,
    ];

    /// The stored `status` string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "QUEUED",
            Status::Reserved => "RESERVED",
            Status::Submitting => "SUBMITTING",
            Status::Assigned => "ASSIGNED",
            Status::RetryAfter => "RETRY_AFTER",
            Status::FailedTerminal => "FAILED_TERMINAL",
        }
    }
}

/// How many rows sit in one outbox status, and how long the oldest of them has
/// been there.
#[derive(Debug, Clone, Copy)]
pub struct StatusDepth {
    /// Rows currently in this status.
    pub depth: i64,
    /// Age of the oldest row in this status, in seconds. `None` when empty.
    pub oldest_age_secs: Option<f64>,
}

/// Depth and oldest-row age per status, in one grouped scan.
///
/// Statuses with no rows come back zero-depth, so a gauge built from this never
/// disappears mid-incident.
pub async fn depth_by_status(pool: &PgPool) -> Result<Vec<(Status, StatusDepth)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT status, count(*) AS depth, \
                extract(epoch from now() - min(created_at))::float8 AS oldest_age_secs \
         FROM username_reservations GROUP BY status",
    )
    .fetch_all(pool)
    .await?;

    let mut observed = std::collections::BTreeMap::new();
    for row in &rows {
        let status: String = row.try_get("status")?;
        observed.insert(
            status,
            StatusDepth {
                depth: row.try_get("depth")?,
                oldest_age_secs: row.try_get("oldest_age_secs")?,
            },
        );
    }
    Ok(Status::ALL
        .into_iter()
        .map(|status| {
            let depth = observed.remove(status.as_str()).unwrap_or(StatusDepth {
                depth: 0,
                oldest_age_secs: None,
            });
            (status, depth)
        })
        .collect())
}

/// A new reservation to persist, built by the API from a validated request.
pub struct NewReservation {
    /// Authenticated JWT subject (`0x`-hex sr25519). Equals `candidate_account_id`.
    pub account_id: String,
    /// Beneficiary SS58 address (the on-chain candidate/consumer account).
    pub candidate_account_id: String,
    /// Base name (no digits).
    pub base: String,
    /// Two-digit discriminator, `01`..`99`.
    pub digits: String,
    /// `base.digits` — the full on-chain lite username (idempotency key).
    pub full_username: String,
    /// sr25519 candidate signature (64 bytes).
    pub candidate_signature: Vec<u8>,
    /// Bandersnatch ring-VRF key/proof.
    pub ring_vrf_key: Vec<u8>,
    /// Proof of ownership (64 bytes).
    pub proof_of_ownership: Vec<u8>,
    /// sr25519 consumer-registration signature (64 bytes).
    pub consumer_registration_signature: Vec<u8>,
    /// secp256k1 uncompressed ECDH public key (65 bytes).
    pub identifier_key: Vec<u8>,
    /// Optional DotNS reservation signature.
    pub dotns_signature: Option<Vec<u8>>,
    /// Optional DotNS `signedAt` (unix seconds).
    pub dotns_signed_at: Option<i64>,
    /// Optional base label parked for later full-person registration.
    pub reserved_username: Option<String>,
}

/// Why an insert failed.
#[derive(Debug, thiserror::Error)]
pub enum InsertError {
    /// The `full_username` is already reserved/registered (unique violation).
    #[error("username already taken")]
    Conflict,
    /// Any other database failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Insert a fresh `RESERVED` reservation. Maps a unique violation on
/// `full_username` to [`InsertError::Conflict`] (the name was taken in a race).
///
/// Generic over the executor so it runs either directly on the pool or inside
/// a caller's transaction (e.g. the DeviceCheck reserve-and-mark path).
pub async fn insert<'e, E>(executor: E, r: &NewReservation) -> Result<i64, InsertError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    insert_with(executor, r, Status::Reserved, 1).await
}

/// Insert a fresh `QUEUED` reservation with its initial priority group
/// (the `QUEUE_ENABLED` intake lane). Same conflict mapping and executor
/// genericity as [`insert`].
pub async fn insert_queued<'e, E>(
    executor: E,
    r: &NewReservation,
    queue_group: i32,
) -> Result<i64, InsertError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    insert_with(executor, r, Status::Queued, queue_group).await
}

async fn insert_with<'e, E>(
    executor: E,
    r: &NewReservation,
    status: Status,
    queue_group: i32,
) -> Result<i64, InsertError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query(
        "INSERT INTO username_reservations \
           (account_id, candidate_account_id, base, digits, full_username, \
            candidate_signature, ring_vrf_key, proof_of_ownership, \
            consumer_registration_signature, identifier_key, \
            dotns_signature, dotns_signed_at, reserved_username, \
            status, queue_group) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
         RETURNING id",
    )
    .bind(&r.account_id)
    .bind(&r.candidate_account_id)
    .bind(&r.base)
    .bind(&r.digits)
    .bind(&r.full_username)
    .bind(&r.candidate_signature)
    .bind(&r.ring_vrf_key)
    .bind(&r.proof_of_ownership)
    .bind(&r.consumer_registration_signature)
    .bind(&r.identifier_key)
    .bind(&r.dotns_signature)
    .bind(r.dotns_signed_at)
    .bind(&r.reserved_username)
    .bind(status.as_str())
    .bind(queue_group)
    .fetch_one(executor)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => InsertError::Conflict,
        _ => InsertError::Db(e),
    })?;
    Ok(row.try_get::<i64, _>("id")?)
}

/// Return every discriminator already protected by the outbox for `base`.
///
/// All statuses count because `full_username` remains permanently unique even
/// for terminal rows. Advertising one of those digits as available would make
/// a subsequent insert fail despite the availability response.
pub async fn allocated_discriminators(
    pool: &PgPool,
    base: &str,
) -> Result<BTreeSet<u8>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT digits FROM username_reservations \
         WHERE base = $1",
    )
    .bind(base)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let raw = row.try_get::<String, _>("digits")?;
            raw.parse::<u8>()
                .map_err(|error| sqlx::Error::ColumnDecode {
                    index: "digits".to_string(),
                    source: Box::new(error),
                })
        })
        .collect()
}

/// A reservation the writer submits, with the on-chain attestation payload.
#[derive(Debug, Clone)]
pub struct Reservation {
    /// Primary key.
    pub id: i64,
    /// `base.digits` — the on-chain lite username.
    pub full_username: String,
    /// Beneficiary SS58 address.
    pub candidate_account_id: String,
    /// sr25519 candidate signature (64 bytes).
    pub candidate_signature: Vec<u8>,
    /// Bandersnatch ring-VRF key/proof.
    pub ring_vrf_key: Vec<u8>,
    /// Proof of ownership (64 bytes).
    pub proof_of_ownership: Vec<u8>,
    /// sr25519 consumer-registration signature (64 bytes).
    pub consumer_registration_signature: Vec<u8>,
    /// secp256k1 uncompressed ECDH public key (65 bytes).
    pub identifier_key: Vec<u8>,
    /// Optional parked base label.
    pub reserved_username: Option<String>,
    /// Submission attempt count so far.
    pub attempt: i32,
}

const SELECT_COLS: &str = "id, full_username, candidate_account_id, candidate_signature, \
     ring_vrf_key, proof_of_ownership, consumer_registration_signature, identifier_key, \
     reserved_username, attempt";

fn row_to_reservation(row: &sqlx::postgres::PgRow) -> Result<Reservation, sqlx::Error> {
    Ok(Reservation {
        id: row.try_get("id")?,
        full_username: row.try_get("full_username")?,
        candidate_account_id: row.try_get("candidate_account_id")?,
        candidate_signature: row.try_get("candidate_signature")?,
        ring_vrf_key: row.try_get("ring_vrf_key")?,
        proof_of_ownership: row.try_get("proof_of_ownership")?,
        consumer_registration_signature: row.try_get("consumer_registration_signature")?,
        identifier_key: row.try_get("identifier_key")?,
        reserved_username: row.try_get("reserved_username")?,
        attempt: row.try_get("attempt")?,
    })
}

/// Fetch up to `limit` rows that are due for submission: `RESERVED`, or
/// `RETRY_AFTER` whose backoff gate has passed. Oldest first.
///
/// V1 has a single active writer, so a plain ordered select is a correct claim;
/// the scale path (proxy-per-worker) adds `FOR UPDATE SKIP LOCKED`.
pub async fn claim_due(pool: &PgPool, limit: i64) -> Result<Vec<Reservation>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLS} FROM username_reservations \
         WHERE status = 'RESERVED' \
            OR (status = 'RETRY_AFTER' AND (not_before IS NULL OR not_before <= now())) \
         ORDER BY created_at ASC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_reservation).collect()
}

/// Fetch all rows stuck in `SUBMITTING` — drained + reconciled on writer startup
/// (a previous writer may have recorded `SUBMITTING` and died mid-flight).
pub async fn submitting(pool: &PgPool) -> Result<Vec<Reservation>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLS} FROM username_reservations WHERE status = 'SUBMITTING' \
         ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_reservation).collect()
}

/// A lease guard: writer mutations only apply while this holder owns a current,
/// unexpired lease. Realises the spec's "guard every writer DB mutation with
/// `WHERE lease_epoch = <current>`".
#[derive(Debug, Clone)]
pub struct Guard {
    /// Lease row name (e.g. `people-chain-writer`).
    pub lease_name: String,
    /// This writer instance's id.
    pub holder_id: String,
    /// The epoch acquired.
    pub epoch: i64,
}

/// The lease-held guard, appended to every writer mutation. Params `$5,$6,$7`
/// are the lease name, holder id, and epoch (all writer mutations bind them last
/// in that order).
const LEASE_HELD: &str = " AND EXISTS (SELECT 1 FROM writer_lease \
     WHERE name = $5 AND holder_id = $6 AND lease_epoch = $7 AND expires_at > now())";

/// Record `SUBMITTING` with `tx_hash` + `nonce` before awaiting inclusion.
pub async fn mark_submitting(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    tx_hash: &str,
    nonce: i64,
    attempt: i32,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "UPDATE username_reservations \
         SET status='SUBMITTING', tx_hash=$2, nonce=$3, attempt=$4, submitted_at=now(), updated_at=now() \
         WHERE id=$1{LEASE_HELD}"
    );
    let done = sqlx::query(&sql)
        .bind(id)
        .bind(tx_hash)
        .bind(nonce)
        .bind(attempt)
        .bind(&guard.lease_name)
        .bind(&guard.holder_id)
        .bind(guard.epoch)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Mark a reservation confirmed on-chain (terminal success).
pub async fn mark_assigned(pool: &PgPool, guard: &Guard, id: i64) -> Result<bool, sqlx::Error> {
    guarded_status(pool, guard, id, Status::Assigned, None, None).await
}

/// Re-enqueue with backoff: `RETRY_AFTER` until `not_before`.
pub async fn mark_retry(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    not_before: OffsetDateTime,
    attempt: i32,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_status(
        pool,
        guard,
        id,
        Status::RetryAfter,
        Some((not_before, attempt)),
        Some(err),
    )
    .await
}

/// Mark a reservation permanently failed (terminal).
pub async fn mark_failed(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_status(pool, guard, id, Status::FailedTerminal, None, Some(err)).await
}

/// Guarded status transition. `retry` carries `(not_before, attempt)` only for
/// `RETRY_AFTER`; `err` is stored in `last_error` when present.
async fn guarded_status(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    status: Status,
    retry: Option<(OffsetDateTime, i32)>,
    err: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let (not_before, attempt) = match retry {
        Some((nb, a)) => (Some(nb), Some(a)),
        None => (None, None),
    };
    let sql = format!(
        "UPDATE username_reservations \
         SET status=$2, \
             not_before = CASE WHEN $2 = 'RETRY_AFTER' THEN $3 ELSE not_before END, \
             attempt = COALESCE($4, attempt), \
             last_error = COALESCE($8, last_error), \
             updated_at = now() \
         WHERE id=$1{LEASE_HELD}"
    );
    let done = sqlx::query(&sql)
        .bind(id)
        .bind(status.as_str())
        .bind(not_before)
        .bind(attempt)
        .bind(&guard.lease_name)
        .bind(&guard.holder_id)
        .bind(guard.epoch)
        .bind(err)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards [`Status::ALL`] against drifting from the enum: a missing entry
    /// would recreate the vanishing-gauge problem `ALL` exists to prevent. The
    /// exhaustive match stops compiling when a variant is added — extend it and
    /// `ALL` together — and the distinct-count check fails until `ALL` really
    /// contains the new variant rather than a repeat.
    #[test]
    fn all_covers_every_status() {
        for status in Status::ALL {
            match status {
                Status::Queued
                | Status::Reserved
                | Status::Submitting
                | Status::Assigned
                | Status::RetryAfter
                | Status::FailedTerminal => {}
            }
        }
        let distinct: std::collections::BTreeSet<_> =
            Status::ALL.iter().map(|status| status.as_str()).collect();
        assert_eq!(distinct.len(), Status::ALL.len(), "ALL repeats a status");
    }
}
