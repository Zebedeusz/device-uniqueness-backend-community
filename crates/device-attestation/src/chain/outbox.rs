//! Reservation outbox — the durable queue + state machine behind username
//! registration.
//!
//! `POST /api/v1/usernames` inserts a [`Status::Reserved`] row and returns 202;
//! the `device-attestation-chain-writer` binary claims due rows, submits `PeopleLite.attest`, and
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

/// dotNS gateway lane state. The Asset Hub half of a reservation row.
///
/// Advances independently of [`Status`]. A dotNS failure never changes the
/// People registration's status. Absent (`NULL` in the column) means the
/// request carried no `dotns` block, or the row predates the lane.
///
/// ```text
/// PENDING ──People ASSIGNED + gates pass──▶ SUBMITTING ──finalized──▶ RESERVED
/// SUBMITTING ──transient──▶ RETRY_AFTER ──dotns_not_before──▶ (re-picked)
/// PENDING/SUBMITTING ──bad signature / label taken / unbuildable──▶ FAILED_TERMINAL
/// PENDING ──signed_at aged out of MaxValiditySeconds──▶ EXPIRED
/// PENDING/RETRY_AFTER ──People reached FAILED_TERMINAL──▶ ABANDONED
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DotnsStatus {
    /// Carries a dotns block. Not yet submitted to Asset Hub.
    Pending,
    /// Extrinsic built, signed and broadcast. Recorded before awaiting
    /// inclusion.
    Submitting,
    /// Confirmed on Asset Hub, or reconciled as already-owned. Terminal
    /// success.
    Reserved,
    /// Transient failure. Re-enqueued with backoff via `dotns_not_before`.
    RetryAfter,
    /// Permanent failure: bad signature, or label owned elsewhere. Terminal.
    FailedTerminal,
    /// The reservation signature aged out before submission. Terminal. Not
    /// recoverable by the backend. Only the client can re-sign.
    Expired,
    /// The People half failed terminally, so the dotNS half was never
    /// attempted. Terminal.
    ///
    /// Distinct from [`DotnsStatus::FailedTerminal`], which means the dotNS
    /// submission itself was refused. Nothing is wrong with this row's
    /// reservation; it simply has no username to attach a name to.
    /// `claim_dotns_due` only takes rows already `ASSIGNED` on People, so
    /// without this state these rows would sit in `PENDING` forever and count
    /// against `dub_dotns_outbox_depth{status="PENDING"}` — the gauge the
    /// runbook reads as "the lane is stuck".
    Abandoned,
}

impl DotnsStatus {
    /// Every status, so a gauge pass can report `0` for the empty ones.
    pub const ALL: [DotnsStatus; 7] = [
        DotnsStatus::Pending,
        DotnsStatus::Submitting,
        DotnsStatus::Reserved,
        DotnsStatus::RetryAfter,
        DotnsStatus::FailedTerminal,
        DotnsStatus::Expired,
        DotnsStatus::Abandoned,
    ];

    /// The stored `dotns_status` string.
    pub const fn as_str(self) -> &'static str {
        match self {
            DotnsStatus::Pending => "PENDING",
            DotnsStatus::Submitting => "SUBMITTING",
            DotnsStatus::Reserved => "RESERVED",
            DotnsStatus::RetryAfter => "RETRY_AFTER",
            DotnsStatus::FailedTerminal => "FAILED_TERMINAL",
            DotnsStatus::Expired => "EXPIRED",
            DotnsStatus::Abandoned => "ABANDONED",
        }
    }
}

/// How many rows sit in one outbox status, and how long the one that has been
/// there longest has been there.
#[derive(Debug, Clone, Copy)]
pub struct StatusDepth {
    /// Rows currently in this status.
    pub depth: i64,
    /// Seconds the longest-waiting row has been in this status. `None` when
    /// empty.
    pub oldest_age_secs: Option<f64>,
}

/// Depth and longest wait per status, in one grouped scan.
///
/// Statuses with no rows come back zero-depth, so a gauge built from this never
/// disappears mid-incident.
///
/// The age is `now() - min(updated_at)`, not `created_at`: the question is "how
/// long has work been stuck *here*", and every status transition sets
/// `updated_at`. Measuring from creation instead makes a row that was just
/// re-queued read as hours old, and makes the terminal statuses grow without
/// bound. Two known approximations: a `queue_group` re-prioritisation also
/// bumps `updated_at` without a transition, and `updated_at` is shared with the
/// dotNS lane (see [`dotns_depth_by_status`]).
pub async fn depth_by_status(pool: &PgPool) -> Result<Vec<(Status, StatusDepth)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT status, count(*) AS depth, \
                extract(epoch from now() - min(updated_at))::float8 AS oldest_age_secs \
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
    // A request carrying a dotns block enters the Asset Hub lane as PENDING.
    // One without leaves `dotns_status` NULL. The claim scan never picks those.
    let dotns_status = r
        .dotns_signature
        .is_some()
        .then_some(DotnsStatus::Pending.as_str());
    let row = sqlx::query(
        "INSERT INTO username_reservations \
           (account_id, candidate_account_id, base, digits, full_username, \
            candidate_signature, ring_vrf_key, proof_of_ownership, \
            consumer_registration_signature, identifier_key, \
            dotns_signature, dotns_signed_at, reserved_username, \
            status, queue_group, dotns_status) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
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
    .bind(dotns_status)
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
    /// sr25519 dotNS reservation signature, when the request carried one.
    pub dotns_signature: Option<Vec<u8>>,
    /// dotNS `signedAt` (unix seconds), when the request carried one.
    pub dotns_signed_at: Option<i64>,
    /// Asset Hub submission attempt count so far.
    pub dotns_attempt: i32,
}

const SELECT_COLS: &str = "id, full_username, candidate_account_id, candidate_signature, \
     ring_vrf_key, proof_of_ownership, consumer_registration_signature, identifier_key, \
     reserved_username, attempt, dotns_signature, dotns_signed_at, dotns_attempt";

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
        dotns_signature: row.try_get("dotns_signature")?,
        dotns_signed_at: row.try_get("dotns_signed_at")?,
        dotns_attempt: row.try_get("dotns_attempt")?,
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

/// Fetches up to `limit` rows due for **Asset Hub** submission. Oldest first.
///
/// Due means already `ASSIGNED` on People, carrying a dotns block, and past the
/// backoff gate.
///
/// `ASSIGNED` is a hard precondition, not an optimisation. `reserve_name`
/// writes `LiteLabelOwner`, a global claim on the label. A name whose People
/// attestation failed must never be claimed in dotNS.
pub async fn claim_dotns_due(pool: &PgPool, limit: i64) -> Result<Vec<Reservation>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLS} FROM username_reservations \
         WHERE status = 'ASSIGNED' \
           AND (dotns_status = 'PENDING' \
                OR (dotns_status = 'RETRY_AFTER' \
                    AND (dotns_not_before IS NULL OR dotns_not_before <= now()))) \
         ORDER BY created_at ASC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_reservation).collect()
}

/// Fetches all rows stuck in `dotns_status = 'SUBMITTING'`.
///
/// They are reconciled against `LiteLabelOwner` on writer startup. They are
/// never blindly resubmitted.
pub async fn dotns_submitting(pool: &PgPool) -> Result<Vec<Reservation>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {SELECT_COLS} FROM username_reservations \
         WHERE dotns_status = 'SUBMITTING' ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_reservation).collect()
}

/// Depth and longest wait per dotNS status, in one grouped scan.
///
/// `NULL` rows are excluded. They are not in the lane. Counting them would
/// swamp the gauge with every registration that never asked for a dotNS name.
///
/// The age measures time in `dotns_status`, via the same `updated_at` the
/// People lane uses — the two lanes share the column, so a People transition
/// resets it. In practice that is close enough: the People half reaches
/// `ASSIGNED` before the dotNS half is claimable, so the reset lands at the
/// moment this lane starts waiting. A dedicated `dotns_updated_at` is the exact
/// answer if this ever needs to be alerted on tightly.
pub async fn dotns_depth_by_status(
    pool: &PgPool,
) -> Result<Vec<(DotnsStatus, StatusDepth)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT dotns_status, count(*) AS depth, \
                extract(epoch from now() - min(updated_at))::float8 AS oldest_age_secs \
         FROM username_reservations WHERE dotns_status IS NOT NULL GROUP BY dotns_status",
    )
    .fetch_all(pool)
    .await?;

    let mut observed = std::collections::BTreeMap::new();
    for row in &rows {
        let status: String = row.try_get("dotns_status")?;
        observed.insert(
            status,
            StatusDepth {
                depth: row.try_get("depth")?,
                oldest_age_secs: row.try_get("oldest_age_secs")?,
            },
        );
    }
    Ok(DotnsStatus::ALL
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
///
/// Also closes an open dotNS lane on the row as `ABANDONED` — see
/// [`guarded_status`]. The dotNS half can never run once the People half is
/// dead, because there is no username for the name to hang off.
pub async fn mark_failed(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_status(pool, guard, id, Status::FailedTerminal, None, Some(err)).await
}

/// Records `dotns_status = 'SUBMITTING'` with the Asset Hub tx hash before
/// awaiting inclusion.
///
/// A crash mid-flight then reconciles instead of resubmitting.
pub async fn mark_dotns_submitting(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    tx_hash: &str,
    attempt: i32,
) -> Result<bool, sqlx::Error> {
    // `$4` is the retry gate, cleared because the row is no longer waiting on
    // a backoff. It also keeps the four-value-parameter shape `LEASE_HELD`
    // requires. That clause hard-codes `$5,$6,$7`.
    let sql = format!(
        "UPDATE username_reservations \
         SET dotns_status='SUBMITTING', dotns_tx_hash=$2, dotns_attempt=$3, \
             dotns_not_before=$4, updated_at=now() \
         WHERE id=$1{LEASE_HELD}"
    );
    let done = sqlx::query(&sql)
        .bind(id)
        .bind(tx_hash)
        .bind(attempt)
        .bind(None::<OffsetDateTime>)
        .bind(&guard.lease_name)
        .bind(&guard.holder_id)
        .bind(guard.epoch)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Marks the dotNS reservation confirmed on Asset Hub. Terminal success.
pub async fn mark_dotns_reserved(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
) -> Result<bool, sqlx::Error> {
    guarded_dotns_status(pool, guard, id, DotnsStatus::Reserved, None, None).await
}

/// Re-enqueues the dotNS submission with backoff. `RETRY_AFTER` until
/// `not_before`.
pub async fn mark_dotns_retry(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    not_before: OffsetDateTime,
    attempt: i32,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_dotns_status(
        pool,
        guard,
        id,
        DotnsStatus::RetryAfter,
        Some((not_before, attempt)),
        Some(err),
    )
    .await
}

/// Marks the dotNS reservation permanently failed. Terminal. The People
/// registration's `status` is untouched.
pub async fn mark_dotns_failed(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_dotns_status(
        pool,
        guard,
        id,
        DotnsStatus::FailedTerminal,
        None,
        Some(err),
    )
    .await
}

/// Marks the reservation signature aged out. Terminal, never retried.
pub async fn mark_dotns_expired(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    err: &str,
) -> Result<bool, sqlx::Error> {
    guarded_dotns_status(pool, guard, id, DotnsStatus::Expired, None, Some(err)).await
}

/// Guarded dotNS status transition. The Asset Hub twin of [`guarded_status`].
///
/// Deliberately a separate statement set. Sharing one would let a dotNS outcome
/// touch the People `status` column.
async fn guarded_dotns_status(
    pool: &PgPool,
    guard: &Guard,
    id: i64,
    status: DotnsStatus,
    retry: Option<(OffsetDateTime, i32)>,
    err: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let (not_before, attempt) = match retry {
        Some((nb, a)) => (Some(nb), Some(a)),
        None => (None, None),
    };
    let sql = format!(
        "UPDATE username_reservations \
         SET dotns_status=$2, \
             dotns_not_before = CASE WHEN $2 = 'RETRY_AFTER' THEN $3 ELSE dotns_not_before END, \
             dotns_attempt = COALESCE($4, dotns_attempt), \
             dotns_last_error = COALESCE($8, dotns_last_error), \
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

/// Stored on `dotns_last_error` when the People half fails terminally and the
/// dotNS half is closed without ever being attempted.
const DOTNS_ABANDONED_REASON: &str =
    "People registration failed terminally; dotNS reservation never attempted";

/// The dotNS states a terminal People failure closes out. `SUBMITTING` is
/// deliberately absent: that extrinsic is already broadcast, and the
/// `LiteLabelOwner` reconcile owns its outcome. `RESERVED`/`EXPIRED`/
/// `FAILED_TERMINAL` are already terminal, and `NULL` was never in the lane
/// (`NULL IN (…)` is `NULL`, so those rows fall to the `ELSE` branch).
const DOTNS_OPEN_STATES: &str = "('PENDING', 'RETRY_AFTER')";

/// Guarded status transition. `retry` carries `(not_before, attempt)` only for
/// `RETRY_AFTER`; `err` is stored in `last_error` when present.
///
/// A transition to `FAILED_TERMINAL` also closes an open dotNS lane on the same
/// row, in the same statement. `claim_dotns_due` only takes rows already
/// `ASSIGNED` on People, so a dotNS row left `PENDING` under a failed People
/// half is unreachable forever — and would still be counted by
/// `dotns_depth_by_status`, inflating the depth and oldest-age gauges the
/// runbook reads as a stuck lane. Doing it here rather than in a second
/// statement keeps it atomic and under the same lease guard.
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
    // Every SET expression reads the row's pre-update values, so the guards
    // below still see the old `dotns_status` even as the first one rewrites it.
    let closes_dotns = format!("$2 = 'FAILED_TERMINAL' AND dotns_status IN {DOTNS_OPEN_STATES}");
    let sql = format!(
        "UPDATE username_reservations \
         SET status=$2, \
             not_before = CASE WHEN $2 = 'RETRY_AFTER' THEN $3 ELSE not_before END, \
             attempt = COALESCE($4, attempt), \
             last_error = COALESCE($8, last_error), \
             dotns_status = CASE WHEN {closes_dotns} \
                 THEN '{abandoned}' ELSE dotns_status END, \
             dotns_last_error = CASE WHEN {closes_dotns} \
                 THEN '{reason}' ELSE dotns_last_error END, \
             dotns_not_before = CASE WHEN {closes_dotns} \
                 THEN NULL ELSE dotns_not_before END, \
             updated_at = now() \
         WHERE id=$1{LEASE_HELD}",
        abandoned = DotnsStatus::Abandoned.as_str(),
        reason = DOTNS_ABANDONED_REASON,
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

    /// Same guard for the dotNS lane's `ALL`.
    #[test]
    fn dotns_all_covers_every_status() {
        for status in DotnsStatus::ALL {
            match status {
                DotnsStatus::Pending
                | DotnsStatus::Submitting
                | DotnsStatus::Reserved
                | DotnsStatus::RetryAfter
                | DotnsStatus::FailedTerminal
                | DotnsStatus::Expired
                | DotnsStatus::Abandoned => {}
            }
        }
        let distinct: std::collections::BTreeSet<_> = DotnsStatus::ALL
            .iter()
            .map(|status| status.as_str())
            .collect();
        assert_eq!(
            distinct.len(),
            DotnsStatus::ALL.len(),
            "ALL repeats a status"
        );
    }
}
