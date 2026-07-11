//! Shared projection record type, consumer decoding, and single-record writes.
//!
//! Both the one-shot [`crate::bootstrap`] snapshot scan and the
//! [`crate::incremental`] finalized-block indexer decode `Resources::Consumers`
//! values into [`AssignedUsername`] rows and persist them through the same
//! in-transaction [`upsert`]/[`delete_account`] helpers, so the two paths stay
//! byte-for-byte consistent.

use sqlx::{Postgres, Transaction};

use crate::ss58;

/// A decoded assigned username ready for persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedUsername {
    /// Raw 32-byte account identifier.
    pub account_id: [u8; 32],
    /// Account identifier encoded with the connected chain's SS58 prefix.
    pub account_id_ss58: String,
    /// Opaque consumer communication identifier.
    pub identifier_key: [u8; 65],
    /// Full on-chain lite username including its numeric suffix.
    pub lite_username: String,
    /// Lite username portion before the last dot.
    pub lite_base: String,
    /// Decimal suffix after the last dot.
    pub lite_digits: String,
    /// Optional full-person username.
    pub full_username: Option<String>,
    /// Username shown by the API.
    pub display_username: String,
    /// Finalized snapshot hash.
    pub snapshot_hash: [u8; 32],
    /// Finalized snapshot number.
    pub snapshot_number: u64,
}

/// Advisory-lock id shared by bootstrap + incremental sync so only one writer
/// mutates the projection at a time — safe across replicas and rolling deploys.
pub(crate) const PROJECTION_LOCK_ID: i64 = 0x7573_6572_6E61_6D65;

/// Malformed consumer username data.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// Lite username was not valid UTF-8.
    #[error("lite username is not valid UTF-8")]
    LiteUtf8,
    /// Full username was not valid UTF-8.
    #[error("full username is not valid UTF-8")]
    FullUtf8,
    /// Lite username did not end in `.digits`.
    #[error("lite username must contain a non-empty base and numeric suffix")]
    LiteFormat,
    /// Full username was empty.
    #[error("full username must not be empty")]
    EmptyFull,
    /// A username contained a NUL byte, which Postgres `text` cannot store.
    #[error("username contains a NUL byte")]
    NulByte,
}

/// Decode raw `Resources::Consumers` fields into an [`AssignedUsername`].
pub(crate) fn decode_consumer(
    account_id: [u8; 32],
    identifier_key: [u8; 65],
    lite_username: Vec<u8>,
    full_username: Option<Vec<u8>>,
    ss58_prefix: u16,
    snapshot_hash: [u8; 32],
    snapshot_number: u64,
) -> Result<AssignedUsername, DecodeError> {
    let lite_username = String::from_utf8(lite_username).map_err(|_| DecodeError::LiteUtf8)?;
    let (lite_base, lite_digits) = lite_username
        .rsplit_once('.')
        .ok_or(DecodeError::LiteFormat)?;
    if lite_base.is_empty()
        || lite_digits.is_empty()
        || !lite_digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(DecodeError::LiteFormat);
    }
    let lite_base = lite_base.to_string();
    let lite_digits = lite_digits.to_string();
    let full_username = full_username
        .map(|value| String::from_utf8(value).map_err(|_| DecodeError::FullUtf8))
        .transpose()?;
    if full_username.as_deref() == Some("") {
        return Err(DecodeError::EmptyFull);
    }
    if lite_username.contains('\0')
        || full_username
            .as_deref()
            .is_some_and(|full| full.contains('\0'))
    {
        return Err(DecodeError::NulByte);
    }
    let display_username = full_username
        .clone()
        .unwrap_or_else(|| lite_username.clone());
    let account_id_ss58 = ss58::encode(&account_id, ss58_prefix)
        .expect("validated SS58 prefix must encode an account");

    Ok(AssignedUsername {
        account_id,
        account_id_ss58,
        identifier_key,
        lite_username,
        lite_base,
        lite_digits,
        full_username,
        display_username,
        snapshot_hash,
        snapshot_number,
    })
}

/// Upsert one projection row within an open transaction, keyed by account.
pub(crate) async fn upsert(
    tx: &mut Transaction<'_, Postgres>,
    record: &AssignedUsername,
    snapshot_number: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO assigned_usernames (
            account_id, account_id_ss58, identifier_key, lite_username, lite_base,
            lite_digits, full_username, display_username, snapshot_hash, snapshot_number
         ) VALUES ($1, $2, $3, $4, $5, $6::numeric, $7, $8, $9, $10)
         ON CONFLICT (account_id) DO UPDATE SET
            account_id_ss58 = EXCLUDED.account_id_ss58,
            identifier_key = EXCLUDED.identifier_key,
            lite_username = EXCLUDED.lite_username,
            lite_base = EXCLUDED.lite_base,
            lite_digits = EXCLUDED.lite_digits,
            full_username = EXCLUDED.full_username,
            display_username = EXCLUDED.display_username,
            snapshot_hash = EXCLUDED.snapshot_hash,
            snapshot_number = EXCLUDED.snapshot_number,
            updated_at = now()",
    )
    .bind(record.account_id.as_slice())
    .bind(&record.account_id_ss58)
    .bind(record.identifier_key.as_slice())
    .bind(&record.lite_username)
    .bind(&record.lite_base)
    .bind(&record.lite_digits)
    .bind(&record.full_username)
    .bind(&record.display_username)
    .bind(record.snapshot_hash.as_slice())
    .bind(snapshot_number)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Delete one projection row by account within an open transaction.
///
/// Used when an affected account's `Resources::Consumers` entry is absent at
/// the finalized block, reconciling the local row to authoritative chain state.
pub(crate) async fn delete_account(
    tx: &mut Transaction<'_, Postgres>,
    account_id: &[u8; 32],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM assigned_usernames WHERE account_id = $1")
        .bind(account_id.as_slice())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decode_consumer, DecodeError};

    fn decode(lite: &[u8], full: Option<&[u8]>) -> Result<super::AssignedUsername, DecodeError> {
        decode_consumer(
            [1; 32],
            [2; 65],
            lite.to_vec(),
            full.map(<[u8]>::to_vec),
            42,
            [3; 32],
            99,
        )
    }

    #[test]
    fn decodes_lite_username_using_last_dot() {
        let record = decode(b"alice.team.007", None).expect("valid consumer");
        assert_eq!(record.lite_base, "alice.team");
        assert_eq!(record.lite_digits, "007");
        assert_eq!(record.display_username, "alice.team.007");
    }

    #[test]
    fn full_username_shadows_lite_username() {
        let record = decode(b"alice.12", Some(b"Alice Smith")).expect("valid consumer");
        assert_eq!(record.full_username.as_deref(), Some("Alice Smith"));
        assert_eq!(record.display_username, "Alice Smith");
    }

    #[test]
    fn rejects_malformed_usernames() {
        assert_eq!(decode(b"alice", None), Err(DecodeError::LiteFormat));
        assert_eq!(decode(b"alice.xyz", None), Err(DecodeError::LiteFormat));
        assert_eq!(
            decode(&[0xff, b'.', b'1'], None),
            Err(DecodeError::LiteUtf8)
        );
        assert_eq!(
            decode(b"alice.1", Some(&[0xff])),
            Err(DecodeError::FullUtf8)
        );
        assert_eq!(decode(b"al\0ice.1", None), Err(DecodeError::NulByte));
        assert_eq!(
            decode(b"alice.1", Some(b"Al\0ice")),
            Err(DecodeError::NulByte)
        );
    }
}
