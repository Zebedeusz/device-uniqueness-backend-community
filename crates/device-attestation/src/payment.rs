//! Payment lane — the PAYMENT_REQUIRED quote behind the eligibility slice
//! (`docs/plans/active/eligibility-payment.md`, Phase 2).
//!
//! A claim the device gate blocks (seen device, or a missing token in hard
//! mode — the spec's "no error codes; these simply resolve to
//! PAYMENT_REQUIRED") gets a durable [`quote`]: a unique deposit address and
//! the required amount, with the fully-validated claim stored alongside so
//! the Phase-3 watcher can register the username once the deposit lands.
//!
//! Deposit addresses are **threshold-1 multisig accounts of the cold master
//! and a keyless per-subject dummy** —
//! `multi_account_id(sort([master, dummy]), 1)` with
//! `dummy = blake2_256(TAG ‖ subject)`. The dummy is a hash output, not
//! anyone's public key, so of the two signatories only the master can ever
//! dispatch: the service holds no private key, the payer holds no rights
//! over the deposit, and the master-key holder sweeps funds offline with
//! `Multisig.as_multi_threshold_1` (re-deriving the dummy from the row's
//! subject alone — fully deterministic, nothing extra stored).
//!
//! One address per subject, for life (the recorded 2026-08-03 decision, and
//! an accepted divergence): with cumulative-balance detection, any balance
//! left at the address — an unswept earlier payment, a late deposit on an
//! EXPIRED quote, FAILED_CONFLICT support money — counts toward the
//! subject's **next** quote. The mitigation is operational sweep cadence;
//! per-quote delta detection is the recorded fallback if this is abused.

use sqlx::{PgPool, Row as _};
use subxt::utils::AccountId32;

use crate::chain::outbox::NewReservation;
use crate::config::PaymentConfig;

/// The `modlpy/utilisuba` entropy prefix `pallet_multisig::multi_account_id`
/// hashes under (a historical Substrate quirk: `pallet_utility`'s derivative
/// accounts share the same prefix; the encoded shapes differ).
const MULTISIG_PREFIX: &[u8; 16] = b"modlpy/utilisuba";

/// Domain tag for the keyless dummy co-signatory derivation.
const DUMMY_TAG: &[u8] = b"identity/payment-dummy:v1";

fn blake2_256(data: &[u8]) -> [u8; 32] {
    use blake2::digest::consts::U32;
    use blake2::{Blake2b, Digest as _};

    let mut out = [0u8; 32];
    out.copy_from_slice(&Blake2b::<U32>::digest(data));
    out
}

/// The keyless dummy co-signatory for a subject:
/// `blake2_256(TAG ‖ subject)`. A hash output, not a public key — no
/// private key for it exists (finding one means breaking the curve), so the
/// deposit multisig is dispatchable by the master alone.
fn dummy_signatory(subject: &str) -> [u8; 32] {
    let mut entropy = Vec::with_capacity(DUMMY_TAG.len() + subject.len());
    entropy.extend_from_slice(DUMMY_TAG);
    entropy.extend_from_slice(subject.as_bytes());
    blake2_256(&entropy)
}

/// `pallet_multisig::multi_account_id` for two 32-byte signatories at
/// threshold 1 (the pallet takes the full signatory set sorted).
fn multi_account_id_2(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    // SCALE of (&[u8;16], &[AccountId32], u16): raw 16 bytes ‖ compact
    // length (2 = 0x08) ‖ raw 32-byte accounts in sorted order ‖ u16
    // little-endian.
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut entropy = Vec::with_capacity(16 + 1 + 64 + 2);
    entropy.extend_from_slice(MULTISIG_PREFIX);
    entropy.push(0x08);
    entropy.extend_from_slice(lo);
    entropy.extend_from_slice(hi);
    entropy.extend_from_slice(&1u16.to_le_bytes());
    blake2_256(&entropy)
}

/// The subject's deposit account: the threshold-1 multisig of the cold
/// master and the subject's keyless dummy. Deterministic per subject (one
/// address for life — see the module docs for the stale-balance caveat),
/// unbounded (no index space), derivable from the subject alone, and
/// controlled by the master key alone.
pub fn deposit_account(master: &[u8; 32], subject: &str) -> [u8; 32] {
    multi_account_id_2(master, &dummy_signatory(subject))
}

/// Render an account as the SS58 string the quote returns (subxt's generic
/// substrate prefix, matching the SS58 dialect used across this API).
pub fn address_ss58(account: &[u8; 32]) -> String {
    AccountId32(*account).to_string()
}

/// The claim intent stored on a payment request: everything the confirmation
/// hand-off needs to insert the reservation, minus the digit selection
/// (re-selected at confirmation so a quote held for days rarely conflicts).
pub struct ClaimPayload<'a> {
    /// Beneficiary SS58 (validated at claim time).
    pub candidate_account_id: &'a str,
    /// Base username.
    pub base: &'a str,
    /// The digits the claimant asked for, if any.
    pub preferred_digits: Option<&'a str>,
    /// sr25519 candidate signature (verified at claim time).
    pub candidate_signature: &'a [u8],
    /// Bandersnatch ring-VRF key/proof.
    pub ring_vrf_key: &'a [u8],
    /// Proof of ownership.
    pub proof_of_ownership: &'a [u8],
    /// Consumer-registration signature.
    pub consumer_registration_signature: &'a [u8],
    /// secp256k1 identifier key.
    pub identifier_key: &'a [u8],
    /// Optional DotNS block, relayed as validated at claim time.
    pub dotns_signature: Option<&'a [u8]>,
    /// Optional DotNS `signedAt`.
    pub dotns_signed_at: Option<i64>,
    /// Optional DotNS reserved label.
    pub reserved_username: Option<&'a str>,
}

impl<'a> ClaimPayload<'a> {
    /// Borrow the payload out of an assembled reservation (whose digit
    /// selection is deliberately NOT stored) plus the original preference.
    pub fn from_reservation(new: &'a NewReservation, preferred_digits: Option<&'a str>) -> Self {
        Self {
            candidate_account_id: &new.candidate_account_id,
            base: &new.base,
            preferred_digits,
            candidate_signature: &new.candidate_signature,
            ring_vrf_key: &new.ring_vrf_key,
            proof_of_ownership: &new.proof_of_ownership,
            consumer_registration_signature: &new.consumer_registration_signature,
            identifier_key: &new.identifier_key,
            dotns_signature: new.dotns_signature.as_deref(),
            dotns_signed_at: new.dotns_signed_at,
            reserved_username: new.reserved_username.as_deref(),
        }
    }
}

/// The deposit instructions returned on the PAYMENT_REQUIRED outcome.
pub struct Quote {
    /// Unique deposit address (SS58).
    pub payment_address: String,
    /// Required amount in planck, frozen at quote time.
    pub amount_planck: u64,
}

/// Return the subject's active quote, creating one if none exists.
///
/// Idempotent per subject: a re-claim returns the **same** address and the
/// amount frozen when the request was first minted, refreshes the TTL, and
/// re-targets the stored claim to the latest intent (last intent wins — a
/// user who re-claims a different username pays for that one). A concurrent
/// first-claim race is resolved by the one-PENDING-per-subject unique index
/// plus one retry.
pub async fn quote(
    pool: &PgPool,
    config: &PaymentConfig,
    subject: &str,
    payload: &ClaimPayload<'_>,
) -> Result<Quote, sqlx::Error> {
    for attempt in 0..2 {
        // Re-target the existing pending quote (keeps address + amount).
        let existing = sqlx::query(
            "UPDATE payment_requests SET \
               candidate_account_id = $2, base = $3, preferred_digits = $4, \
               candidate_signature = $5, ring_vrf_key = $6, proof_of_ownership = $7, \
               consumer_registration_signature = $8, identifier_key = $9, \
               dotns_signature = $10, dotns_signed_at = $11, reserved_username = $12, \
               expires_at = now() + make_interval(secs => $13), updated_at = now() \
             WHERE account_id = $1 AND status = 'PENDING' \
             RETURNING payment_address, amount_planck",
        )
        .bind(subject)
        .bind(payload.candidate_account_id)
        .bind(payload.base)
        .bind(payload.preferred_digits)
        .bind(payload.candidate_signature)
        .bind(payload.ring_vrf_key)
        .bind(payload.proof_of_ownership)
        .bind(payload.consumer_registration_signature)
        .bind(payload.identifier_key)
        .bind(payload.dotns_signature)
        .bind(payload.dotns_signed_at)
        .bind(payload.reserved_username)
        .bind(config.request_ttl.as_secs() as f64)
        .fetch_optional(pool)
        .await?;
        if let Some(row) = existing {
            return Ok(Quote {
                payment_address: row.try_get("payment_address")?,
                // Negative is impossible (CHECK amount_planck > 0); if it ever
                // happens anyway, quote unpayable rather than free.
                amount_planck: u64::try_from(row.try_get::<i64, _>("amount_planck")?)
                    .unwrap_or(u64::MAX),
            });
        }

        // No pending quote: mint one at the subject's deterministic deposit
        // address. Nothing is allocated or consumed — a lost race costs
        // nothing and the address space is unbounded.
        let address = address_ss58(&deposit_account(&config.master_account, subject));

        let inserted = sqlx::query(
            "INSERT INTO payment_requests \
               (account_id, payment_address, amount_planck, expires_at, \
                candidate_account_id, base, preferred_digits, candidate_signature, \
                ring_vrf_key, proof_of_ownership, consumer_registration_signature, \
                identifier_key, dotns_signature, dotns_signed_at, reserved_username) \
             VALUES ($1, $2, $3, now() + make_interval(secs => $4), \
                     $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
             ON CONFLICT (account_id) WHERE status = 'PENDING' DO NOTHING \
             RETURNING id",
        )
        .bind(subject)
        .bind(&address)
        .bind(i64::try_from(config.amount_planck).unwrap_or(i64::MAX))
        .bind(config.request_ttl.as_secs() as f64)
        .bind(payload.candidate_account_id)
        .bind(payload.base)
        .bind(payload.preferred_digits)
        .bind(payload.candidate_signature)
        .bind(payload.ring_vrf_key)
        .bind(payload.proof_of_ownership)
        .bind(payload.consumer_registration_signature)
        .bind(payload.identifier_key)
        .bind(payload.dotns_signature)
        .bind(payload.dotns_signed_at)
        .bind(payload.reserved_username)
        .fetch_optional(pool)
        .await?;
        if inserted.is_some() {
            return Ok(Quote {
                payment_address: address,
                amount_planck: config.amount_planck,
            });
        }
        // Lost the first-claim race: the other request's row now exists —
        // loop once to re-target it.
        tracing::debug!(
            subject,
            attempt,
            "payment quote insert lost a race; retrying"
        );
    }
    Err(sqlx::Error::Protocol(
        "payment quote could not be created after retry".into(),
    ))
}

// ---------------------------------------------------------------------------
// Watcher (Phase 3): deposit detection + registration hand-off. Runs inside
// the single-instance `device-attestation-chain-writer` loop — read-only on chain, so
// the one-signer nonce invariant is untouched.
// ---------------------------------------------------------------------------

/// Counters from one watch pass, for the writer's log line.
#[derive(Debug, Default, Clone, Copy)]
pub struct WatchStats {
    /// PENDING rows past their TTL flipped to EXPIRED.
    pub expired: u64,
    /// Deposits observed; reservations inserted.
    pub confirmed: u64,
    /// Paid, but the base has no free discriminator — FAILED_CONFLICT (support).
    pub conflicted: u64,
    /// Rows still awaiting a deposit (or skipped on a read failure).
    pub still_pending: u64,
}

impl WatchStats {
    /// Anything worth a log line?
    pub fn acted(&self) -> bool {
        self.expired + self.confirmed + self.conflicted > 0
    }
}

/// One pending request as the watcher sees it.
struct PendingRequest {
    id: i64,
    account_id: String,
    payment_address: String,
    amount_planck: i64,
    candidate_account_id: String,
    base: String,
    preferred_digits: Option<String>,
    candidate_signature: Vec<u8>,
    ring_vrf_key: Vec<u8>,
    proof_of_ownership: Vec<u8>,
    consumer_registration_signature: Vec<u8>,
    identifier_key: Vec<u8>,
    dotns_signature: Option<Vec<u8>>,
    dotns_signed_at: Option<i64>,
    reserved_username: Option<String>,
    /// Snapshot of the row's `updated_at`, guarding the CONFIRMED flip: a
    /// re-claim re-targets the intent and touches `updated_at`, so a flip
    /// carrying a stale payload misses and retries with fresh data.
    updated_at: time::OffsetDateTime,
}

/// Flip PENDING rows past their TTL to EXPIRED (spec FR-008/SC-006). A
/// deposit observed later on an EXPIRED request is deliberately not consumed
/// — the row is the support/refund record.
pub async fn expire_pending(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE payment_requests SET status = 'EXPIRED', updated_at = now() \
         WHERE status = 'PENDING' AND expires_at <= now()",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// One watch pass: expire, then check every PENDING address's free balance
/// and hand confirmed deposits to the outbox. Detection is **cumulative
/// balance ≥ required** (the plan's recorded divergence from exact-transfer
/// matching); an unreadable balance skips the row — never confirms, never
/// errors the pass (fail closed on money).
pub async fn watch_pass(pool: &PgPool, chain: &crate::ChainClient) -> anyhow::Result<WatchStats> {
    use std::str::FromStr as _;

    let mut stats = WatchStats {
        expired: expire_pending(pool).await?,
        ..WatchStats::default()
    };
    let rows = fetch_pending(pool).await?;
    for row in rows {
        // A row that cannot even be parsed must not poison the pass: rows
        // are walked in `created_at` order, so erroring here would freeze
        // detection for every request created after it, forever.
        let account = match AccountId32::from_str(&row.payment_address) {
            Ok(account) => account,
            Err(e) => {
                tracing::error!(
                    id = row.id,
                    address = %row.payment_address,
                    error = %e,
                    "stored payment_address invalid; skipping row (support)"
                );
                stats.still_pending += 1;
                continue;
            }
        };
        let balance = match chain.free_balance(account.0).await {
            Ok(balance) => balance,
            Err(e) => {
                tracing::warn!(id = row.id, error = %e, "payment balance read failed; skipping");
                stats.still_pending += 1;
                continue;
            }
        };
        if balance < u128::try_from(row.amount_planck).unwrap_or(u128::MAX) {
            stats.still_pending += 1;
            continue;
        }
        match confirm_request(pool, chain, &row).await? {
            ConfirmOutcome::Confirmed(reservation_id) => {
                tracing::info!(
                    id = row.id,
                    reservation_id,
                    base = %row.base,
                    "payment confirmed; reservation handed to the outbox"
                );
                stats.confirmed += 1;
            }
            ConfirmOutcome::Exhausted => {
                tracing::error!(
                    id = row.id,
                    base = %row.base,
                    "payment confirmed but no discriminator is free; FAILED_CONFLICT (support)"
                );
                stats.conflicted += 1;
            }
            // A digit race or a concurrent handler: the row is retried on the
            // next pass (the deposit is not going anywhere).
            ConfirmOutcome::DigitRace | ConfirmOutcome::AlreadyHandled => {
                stats.still_pending += 1;
            }
        }
    }
    Ok(stats)
}

/// Outcome of one confirmation attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// The request is CONFIRMED and the reservation (id) is in the outbox.
    Confirmed(i64),
    /// Someone else moved the row out of PENDING first — nothing done.
    AlreadyHandled,
    /// The selected discriminator was taken concurrently; retry next pass.
    DigitRace,
    /// No discriminator is free for the base — FAILED_CONFLICT recorded.
    Exhausted,
}

/// Hand a paid request to the outbox: re-select the discriminator against the
/// current chain + outbox state (the preferred digits if still free, else a
/// random free one — a paid registration is never failed for digit contention
/// short of full exhaustion), then flip PENDING → CONFIRMED and insert the
/// `RESERVED` row in one transaction. The flip is guarded by the status AND
/// the snapshot's `updated_at`, so it is idempotent and a concurrent re-claim
/// re-target (which touches `updated_at`) makes a stale-payload flip lose
/// cleanly — retried next pass with fresh data; a conflict rolls everything
/// back.
async fn confirm_request(
    pool: &PgPool,
    chain: &crate::ChainClient,
    row: &PendingRequest,
) -> anyhow::Result<ConfirmOutcome> {
    use rand::seq::SliceRandom as _;

    let (on_chain, in_outbox) = tokio::try_join!(
        async { chain.taken_discriminators(&row.base).await },
        async {
            crate::chain::outbox::allocated_discriminators(pool, &row.base)
                .await
                .map_err(anyhow::Error::from)
        },
    )?;
    let mut taken = on_chain;
    taken.extend(in_outbox);

    let preferred = row
        .preferred_digits
        .as_deref()
        .and_then(|p| p.parse::<u8>().ok())
        .filter(|d| !taken.contains(d));
    let digit = match preferred {
        Some(digit) => Some(digit),
        None => crate::usernames::available_digits(&taken)
            .choose(&mut rand::rngs::OsRng)
            .copied(),
    };
    let Some(digit) = digit else {
        let marked = sqlx::query(
            "UPDATE payment_requests SET status = 'FAILED_CONFLICT', updated_at = now() \
             WHERE id = $1 AND status = 'PENDING' AND updated_at = $2",
        )
        .bind(row.id)
        .bind(row.updated_at)
        .execute(pool)
        .await?;
        // A re-target raced this exhaustion verdict: the verdict is about the
        // OLD intent, so leave the fresh one PENDING for the next pass.
        return Ok(if marked.rows_affected() == 0 {
            ConfirmOutcome::AlreadyHandled
        } else {
            ConfirmOutcome::Exhausted
        });
    };
    let digits = format!("{digit:02}");

    let new = NewReservation {
        account_id: row.account_id.clone(),
        candidate_account_id: row.candidate_account_id.clone(),
        base: row.base.clone(),
        digits: digits.clone(),
        full_username: format!("{}.{digits}", row.base),
        candidate_signature: row.candidate_signature.clone(),
        ring_vrf_key: row.ring_vrf_key.clone(),
        proof_of_ownership: row.proof_of_ownership.clone(),
        consumer_registration_signature: row.consumer_registration_signature.clone(),
        identifier_key: row.identifier_key.clone(),
        dotns_signature: row.dotns_signature.clone(),
        dotns_signed_at: row.dotns_signed_at,
        reserved_username: row.reserved_username.clone(),
    };

    let mut tx = pool.begin().await?;
    let flipped = sqlx::query(
        "UPDATE payment_requests SET status = 'CONFIRMED', confirmed_at = now(), \
         updated_at = now() \
         WHERE id = $1 AND status = 'PENDING' AND updated_at = $2 RETURNING id",
    )
    .bind(row.id)
    .bind(row.updated_at)
    .fetch_optional(&mut *tx)
    .await?;
    if flipped.is_none() {
        return Ok(ConfirmOutcome::AlreadyHandled);
    }
    match crate::chain::outbox::insert(&mut *tx, &new).await {
        Ok(reservation_id) => {
            tx.commit().await?;
            Ok(ConfirmOutcome::Confirmed(reservation_id))
        }
        Err(crate::chain::outbox::InsertError::Conflict) => Ok(ConfirmOutcome::DigitRace),
        Err(crate::chain::outbox::InsertError::Db(e)) => Err(e.into()),
    }
}

/// Confirm one PENDING request by id, bypassing the balance check — the
/// entry the live tests drive, and the support path for manually honoring a
/// payment the cumulative-balance detection cannot see. `None` when the id
/// is not a PENDING request.
pub async fn confirm_by_id(
    pool: &PgPool,
    chain: &crate::ChainClient,
    id: i64,
) -> anyhow::Result<Option<ConfirmOutcome>> {
    let Some(row) = fetch_pending(pool)
        .await?
        .into_iter()
        .find(|row| row.id == id)
    else {
        return Ok(None);
    };
    confirm_request(pool, chain, &row).await.map(Some)
}

async fn fetch_pending(pool: &PgPool) -> Result<Vec<PendingRequest>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, account_id, payment_address, amount_planck, candidate_account_id, base, \
                preferred_digits, candidate_signature, ring_vrf_key, proof_of_ownership, \
                consumer_registration_signature, identifier_key, dotns_signature, \
                dotns_signed_at, reserved_username, updated_at \
         FROM payment_requests WHERE status = 'PENDING' ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(PendingRequest {
                id: row.try_get("id")?,
                account_id: row.try_get("account_id")?,
                payment_address: row.try_get("payment_address")?,
                amount_planck: row.try_get("amount_planck")?,
                candidate_account_id: row.try_get("candidate_account_id")?,
                base: row.try_get("base")?,
                preferred_digits: row.try_get("preferred_digits")?,
                candidate_signature: row.try_get("candidate_signature")?,
                ring_vrf_key: row.try_get("ring_vrf_key")?,
                proof_of_ownership: row.try_get("proof_of_ownership")?,
                consumer_registration_signature: row.try_get("consumer_registration_signature")?,
                identifier_key: row.try_get("identifier_key")?,
                dotns_signature: row.try_get("dotns_signature")?,
                dotns_signed_at: row.try_get("dotns_signed_at")?,
                reserved_username: row.try_get("reserved_username")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// `GET /api/v1/usernames/payment-status` (Phase 4): the client's window into
// the payment lane. Mounted only when the lane is enabled (unmounted → the
// global JSON 404, the registration/queue precedent).
// ---------------------------------------------------------------------------

/// The spec-exact payment-status body: two states, nothing else.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct PaymentStatusResponse {
    /// `"PENDING"` — keep polling; `"CONFIRMED"` — payment detected,
    /// registration in progress.
    #[schema(example = "PENDING")]
    pub status: String,
}

/// Payment status for the authenticated account's most recent request.
#[utoipa::path(
    get,
    path = "/api/v1/usernames/payment-status",
    tag = "Usernames",
    security(("bearer_jwt" = [])),
    description = "Whether the deposit for the caller's PAYMENT_REQUIRED claim has been detected \
      on-chain. Poll after receiving `registrationOutcome: \"PAYMENT_REQUIRED\"`. This route \
      exists only on deployments with the payment lane enabled (`PAYMENT_LANE_ENABLED`); \
      disabled, the path serves the standard plain-text 404. An expired quote answers 404 — \
      re-claim for fresh deposit instructions.",
    responses(
        (status = 200, description = "`PENDING`: not yet detected, keep polling. `CONFIRMED`: \
          deposit detected, username registration is in progress (~15s).",
         body = PaymentStatusResponse,
         example = json!({ "status": "PENDING" })),
        (status = 401, description = "Missing or invalid bearer token.",
         body = serde_json::Value),
        (status = 404, description = "No active payment request for this account (JSON body), or \
          the deployment runs payment-disabled (plain-text body).",
         body = serde_json::Value,
         example = json!({ "error": "No active payment request" })),
        (status = 429, description = "Subject rate limit exceeded (with `Retry-After`).",
         body = serde_json::Value)
    )
)]
pub async fn status(
    axum::extract::State(state): axum::extract::State<crate::http::state::AppState>,
    auth: http_common::AuthSubject,
) -> crate::usernames::error::UsernamesResult<axum::Json<PaymentStatusResponse>> {
    use crate::usernames::error::UsernamesError;

    // Own bucket, subject-keyed (the queue-status precedent): polling here
    // must not eat the claim path's quota.
    let key = format!("/api/v1/usernames/payment-status:{}", auth.subject);
    if !state.limiter.allow(&key) {
        return Err(UsernamesError::RateLimited {
            retry_after_secs: state.config.auth_rate_window.as_secs(),
        });
    }
    let row = sqlx::query(
        "SELECT status FROM payment_requests WHERE account_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(&auth.subject)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| UsernamesError::Internal(e.into()))?;
    let stored: Option<String> = match row {
        Some(row) => Some(
            row.try_get("status")
                .map_err(|e: sqlx::Error| UsernamesError::Internal(e.into()))?,
        ),
        None => None,
    };
    let Some(status) = wire_status(stored.as_deref()) else {
        return Err(UsernamesError::NoPaymentRequest);
    };
    Ok(axum::Json(PaymentStatusResponse {
        status: status.to_string(),
    }))
}

/// Map a stored request status to the spec's two-state wire value; `None` =
/// no ACTIVE request (404 — the client re-claims for a fresh quote).
/// FAILED_CONFLICT deliberately reads as CONFIRMED: the deposit WAS observed
/// — a 404 would push the client to re-claim and pay twice; the stuck
/// registration is a support case, not a fresh quote.
fn wire_status(stored: Option<&str>) -> Option<&'static str> {
    match stored {
        Some("PENDING") => Some("PENDING"),
        Some("CONFIRMED") | Some("FAILED_CONFLICT") => Some("CONFIRMED"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subxt::ext::codec::Encode as _;

    /// The manual byte layout must equal SCALE-encoding the pallet's exact
    /// tuple `(b"modlpy/utilisuba", who, threshold)` with `who` the sorted
    /// signatory vec — pinning `pallet_multisig::multi_account_id`
    /// equivalence. Exercised with several masters so the dummy lands on
    /// both sides of the sort and the ordering branch is covered.
    #[test]
    fn deposit_account_matches_the_pallet_multisig_tuple_encoding() {
        for master in [[0x00_u8; 32], [0x01; 32], [0xAB; 32], [0xFF; 32]] {
            for subject in ["0xsubject-one", "0xsubject-two", ""] {
                let dummy = dummy_signatory(subject);
                let mut pair = [master, dummy];
                pair.sort();
                let who: Vec<AccountId32> = pair.iter().map(|a| AccountId32(*a)).collect();
                let tuple = (MULTISIG_PREFIX, who, 1u16).encode();
                assert_eq!(
                    deposit_account(&master, subject),
                    blake2_256(&tuple),
                    "subject {subject:?}"
                );
            }
        }
    }

    #[test]
    fn deposit_account_is_deterministic_per_subject_and_distinct_across_them() {
        let master = [1u8; 32];
        let other_master = [2u8; 32];
        let a = deposit_account(&master, "alice");
        assert_eq!(
            a,
            deposit_account(&master, "alice"),
            "one subject, one address — the sweep re-derives it from the row"
        );
        assert_ne!(
            a,
            deposit_account(&master, "bob"),
            "subject must vary the address"
        );
        assert_ne!(
            a,
            deposit_account(&other_master, "alice"),
            "master must vary the address"
        );
        assert_ne!(a, master, "the deposit account must differ from the master");
        assert_ne!(
            dummy_signatory("alice"),
            master,
            "the dummy must differ from the master"
        );
    }

    #[test]
    fn payment_status_maps_failed_conflict_to_confirmed_and_expired_to_none() {
        assert_eq!(wire_status(Some("PENDING")), Some("PENDING"));
        assert_eq!(wire_status(Some("CONFIRMED")), Some("CONFIRMED"));
        // The deposit WAS observed: never push a paid user to a fresh quote.
        assert_eq!(wire_status(Some("FAILED_CONFLICT")), Some("CONFIRMED"));
        assert_eq!(wire_status(Some("EXPIRED")), None);
        assert_eq!(wire_status(Some("something-else")), None);
        assert_eq!(wire_status(None), None);
    }

    #[test]
    fn quoted_address_round_trips_through_ss58() {
        use std::str::FromStr as _;
        let account = deposit_account(&[9u8; 32], "0xroundtrip");
        let ss58 = address_ss58(&account);
        assert_eq!(AccountId32::from_str(&ss58).expect("valid ss58").0, account);
    }
}
