//! Registration eligibility — the claim-time decision between the spec's
//! `INSTANT` / `QUEUED` / `PAYMENT_REQUIRED` lanes
//! (`docs/plans/active/eligibility-payment.md`).
//!
//! Phase 1 scope: the voucher lane. A valid `lifetimePoUDVoucher` on
//! `POST /api/v1/usernames` resolves the claim **before** the DeviceCheck/PoUD
//! gate and the registration queue (spec precedence): it registers directly
//! through the reservation outbox and no device bit is spent. A spent,
//! unknown, or expired voucher rejects the claim outright (400) — it never
//! falls through to another lane, so a voucher error can't be farmed to probe
//! the device gate. Vouchers are stored hash-only (threat-model finding M1);
//! [`redeem_and_reserve`] burns the voucher and inserts the reservation in one
//! transaction, so a username conflict never consumes the voucher and a
//! consumed voucher always has its reservation.

use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

use crate::chain::outbox::{self, InsertError, NewReservation};

/// Resolved state of a submitted voucher key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoucherState {
    /// Known, unconsumed, and within its validity window.
    Redeemable,
    /// Known but already consumed (`used_at` set).
    Spent,
    /// Known but past `expires_at`.
    Expired,
    /// No such key hash.
    Unknown,
}

/// Which claim lane the eligibility decision selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Voucher bypass: reserve directly (no PoUD gate, no queue), report
    /// `registrationOutcome: "INSTANT"`.
    Instant,
    /// No voucher: the standard path (DeviceCheck gate, then queue/direct
    /// intake) decides the outcome.
    Standard,
}

/// A voucher that cannot be redeemed; rejects the claim (400).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoucherError {
    /// The key is not in the voucher table.
    Unknown,
    /// The key was already consumed.
    Spent,
    /// The key is past its expiry.
    Expired,
}

/// The Phase-1 eligibility decision table: voucher precedence over every
/// other signal. `None` = no voucher submitted → the standard path; a
/// submitted voucher either redeems (`Instant`) or rejects the claim —
/// never falls through. (Phase 2 grows this with the device-gate and
/// payment-lane inputs.)
pub fn decide(voucher: Option<VoucherState>) -> Result<Lane, VoucherError> {
    match voucher {
        None => Ok(Lane::Standard),
        Some(VoucherState::Redeemable) => Ok(Lane::Instant),
        Some(VoucherState::Spent) => Err(VoucherError::Spent),
        Some(VoucherState::Expired) => Err(VoucherError::Expired),
        Some(VoucherState::Unknown) => Err(VoucherError::Unknown),
    }
}

/// sha256 of the voucher key exactly as submitted on the wire (the base64url
/// string's UTF-8 bytes). The only form ever persisted or compared.
pub fn key_hash(key: &str) -> Vec<u8> {
    Sha256::digest(key.as_bytes()).to_vec()
}

/// Resolve the state of a submitted voucher key.
pub async fn voucher_state(pool: &PgPool, key: &str) -> Result<VoucherState, sqlx::Error> {
    let row =
        sqlx::query("SELECT used_at, expires_at FROM registration_vouchers WHERE key_hash = $1")
            .bind(key_hash(key))
            .fetch_optional(pool)
            .await?;
    let Some(row) = row else {
        return Ok(VoucherState::Unknown);
    };
    let used_at: Option<OffsetDateTime> = row.try_get("used_at")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    Ok(if used_at.is_some() {
        VoucherState::Spent
    } else if expires_at <= OffsetDateTime::now_utc() {
        VoucherState::Expired
    } else {
        VoucherState::Redeemable
    })
}

/// Why an INSTANT redeem failed.
#[derive(Debug, thiserror::Error)]
pub enum RedeemError {
    /// The full username was taken concurrently; the voucher is NOT consumed.
    #[error("username already taken")]
    Conflict,
    /// The voucher stopped being redeemable between the state read and the
    /// burn (lost a concurrent redeem race, or expired in between).
    #[error("voucher no longer redeemable")]
    Voucher(VoucherError),
    /// Any other database failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Burn the voucher and insert the `RESERVED` reservation in one transaction.
///
/// The burn is a conditional UPDATE (`used_at IS NULL AND expires_at >
/// now()`), so a concurrent redeem of the same key loses cleanly; the
/// reservation insert shares the transaction, so a username conflict rolls
/// the burn back and the voucher stays redeemable.
pub async fn redeem_and_reserve(
    pool: &PgPool,
    key: &str,
    new: &NewReservation,
) -> Result<i64, RedeemError> {
    let mut tx = pool.begin().await?;
    let burned = sqlx::query(
        "UPDATE registration_vouchers SET used_at = now() \
         WHERE key_hash = $1 AND used_at IS NULL AND expires_at > now() \
         RETURNING key_hash",
    )
    .bind(key_hash(key))
    .fetch_optional(&mut *tx)
    .await?;
    if burned.is_none() {
        drop(tx);
        // Lost the redeemability between the gate's state read and the burn;
        // re-read to report the accurate reason (a re-read `Redeemable` is a
        // serialization anomaly — report it as the spent race it lost).
        let reason = match voucher_state(pool, key).await? {
            VoucherState::Unknown => VoucherError::Unknown,
            VoucherState::Expired => VoucherError::Expired,
            VoucherState::Spent | VoucherState::Redeemable => VoucherError::Spent,
        };
        return Err(RedeemError::Voucher(reason));
    }
    let id = outbox::insert(&mut *tx, new).await.map_err(|e| match e {
        InsertError::Conflict => RedeemError::Conflict,
        InsertError::Db(e) => RedeemError::Db(e),
    })?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_table_is_total_and_voucher_errors_never_fall_through() {
        assert_eq!(decide(None), Ok(Lane::Standard));
        assert_eq!(decide(Some(VoucherState::Redeemable)), Ok(Lane::Instant));
        assert_eq!(decide(Some(VoucherState::Spent)), Err(VoucherError::Spent));
        assert_eq!(
            decide(Some(VoucherState::Expired)),
            Err(VoucherError::Expired)
        );
        assert_eq!(
            decide(Some(VoucherState::Unknown)),
            Err(VoucherError::Unknown)
        );
    }

    #[test]
    fn key_hash_is_sha256_of_the_wire_string() {
        // Pinned vector: sha256("abc"). The mint CLI and the redeem path must
        // agree on hashing the submitted string's bytes (no decode step).
        assert_eq!(
            hex::encode(key_hash("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(key_hash("").len(), 32);
    }
}
