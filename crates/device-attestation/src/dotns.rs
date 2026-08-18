//! dotNS gateway payload construction. The offline half of the Asset Hub lane.
//!
//! Everything here is pure. No chain, no database. It exists so the backend can
//! decide, before spending an extrinsic, whether a stored `dotns` block will be
//! accepted by `DotnsGateway::reserve_name` on Asset Hub.
//!
//! Three concerns:
//! - [`reservation_message`] / [`verify_reservation_signature`] reconstruct the
//!   payload the candidate signed and check it. They mirror
//!   `Pallet::construct_reservation_message` in `paritytech/individuality`
//!   (`pallets/dotns-gateway`). They are cross-checked against the golden
//!   vectors in this module's tests.
//! - [`check_reserve_name_shape`] is the boot guard. The two live Asset Hubs
//!   run *different* `reserve_name` extrinsics. PreviewNet takes `signed_at`.
//!   Paseo next still takes `ring_vrf_key` + `proof_of_ownership`. The writer
//!   asserts which one it is connected to instead of trusting configuration.
//! - [`reservation_expired`] is the freshness window. It is evaluated against
//!   the chain's own `MaxValiditySeconds` constant.
//!
//! The candidate signature deliberately commits to the **base** of the lite
//! label only ([`lite_base`]), never the full `base.digits`. That is what lets
//! the client sign before the backend allocates a discriminator.

use subxt::ext::codec::Encode as _;

/// Message prefix, matching `RESERVE_MSG_PREFIX` in the gateway pallet.
pub const RESERVE_MSG_PREFIX: &[u8] = b"pop:dotns-gateway:reserve";

/// `DotnsGateway::reserve_name`'s argument names, in order, on the runtime this
/// writer targets.
///
/// Paseo next Asset Hub (`next-asset-hub-paseo` 2000033) declares a different
/// list: `candidate, candidate_signature, ring_vrf_key, proof_of_ownership,
/// lite_label, chat_key, reserved_base_label`. It has no `signed_at` at all.
/// [`check_reserve_name_shape`] is what stops the writer submitting against it.
pub const RESERVE_NAME_FIELDS: [&str; 6] = [
    "candidate",
    "candidate_signature",
    "lite_label",
    "chat_key",
    "reserved_base_label",
    "signed_at",
];

/// The connected Asset Hub's `reserve_name` is not the extrinsic this writer
/// builds.
#[derive(Debug, thiserror::Error)]
#[error(
    "DotnsGateway::reserve_name shape mismatch: this writer builds [{expected}], \
     the connected Asset Hub declares [{found}]. This chain runs a different \
     dotns-gateway; point ASSET_HUB_RPC_URL at one with the signed_at variant, \
     or leave DOTNS_GATEWAY_ENABLED off for this environment."
)]
pub struct ReserveNameShapeError {
    /// Comma-separated argument names this writer encodes.
    expected: String,
    /// Comma-separated argument names the connected runtime declares.
    found: String,
}

/// Returns the bytes of a lite label before the first `.`, or the whole label
/// when it has none.
///
/// Mirrors `BaseLabel::lite_base()` in the gateway pallet. Callers building the
/// reservation message pass this. They never pass the full `base.digits` label.
pub fn lite_base(lite_label: &str) -> &str {
    match lite_label.split_once('.') {
        Some((base, _)) => base,
        None => lite_label,
    }
}

/// Builds the payload a candidate signs to authorise a lite-name reservation.
///
/// SCALE tuple of `(prefix, candidate, attester, username_base, chat_key,
/// reserved_base_label, signed_at)`. The two accounts are raw 32-byte values.
/// The three byte strings are length-prefixed.
///
/// `attester` is the extrinsic's **signed origin**. That is the proxied primary
/// when the writer submits through `Proxy.proxy`. It is the account
/// `GET /api/v1/attester` returns.
pub fn reservation_message(
    candidate: &[u8; 32],
    attester: &[u8; 32],
    username_base: &[u8],
    chat_key: &[u8],
    reserved_base_label: Option<&[u8]>,
    signed_at: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        RESERVE_MSG_PREFIX.len() + 32 + 32 + username_base.len() + chat_key.len() + 48,
    );
    RESERVE_MSG_PREFIX.encode_to(&mut out);
    out.extend_from_slice(candidate);
    out.extend_from_slice(attester);
    username_base.encode_to(&mut out);
    chat_key.encode_to(&mut out);
    reserved_base_label.encode_to(&mut out);
    signed_at.encode_to(&mut out);
    out
}

/// Verifies a stored `dotns.signature` against the reservation payload.
///
/// Returns `false` for anything the pallet would reject as
/// `InvalidAttestationSignature`. That includes a signature that is not 64
/// bytes.
#[allow(clippy::too_many_arguments)]
pub fn verify_reservation_signature(
    signature: &[u8],
    candidate: &[u8; 32],
    attester: &[u8; 32],
    username_base: &[u8],
    chat_key: &[u8],
    reserved_base_label: Option<&[u8]>,
    signed_at: u64,
) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let message = reservation_message(
        candidate,
        attester,
        username_base,
        chat_key,
        reserved_base_label,
        signed_at,
    );
    subxt_signer::sr25519::verify(
        &subxt_signer::sr25519::Signature(signature),
        message,
        &subxt_signer::sr25519::PublicKey(*candidate),
    )
}

/// Whether a reservation signature has aged out of the pallet's validity
/// window.
///
/// The pallet requires `now <= signed_at + MaxValiditySeconds`. This is the
/// negation.
///
/// `max_validity_secs` comes from the connected chain's constant, never from
/// configuration. The backend cannot re-sign. Getting this wrong either burns
/// an extrinsic on a doomed call or discards a still-valid reservation.
pub fn reservation_expired(signed_at: i64, max_validity_secs: u64, now: i64) -> bool {
    let deadline = signed_at.saturating_add_unsigned(max_validity_secs);
    now > deadline
}

/// Checks the connected Asset Hub's `reserve_name` arguments against
/// [`RESERVE_NAME_FIELDS`].
///
/// Names, in order. A positional-only check would accept a runtime that renamed
/// an argument. A set check would accept one that reordered it.
pub fn check_reserve_name_shape<S: AsRef<str>>(fields: &[S]) -> Result<(), ReserveNameShapeError> {
    let matches = fields.len() == RESERVE_NAME_FIELDS.len()
        && fields
            .iter()
            .zip(RESERVE_NAME_FIELDS)
            .all(|(found, expected)| found.as_ref() == expected);
    if matches {
        return Ok(());
    }
    Err(ReserveNameShapeError {
        expected: RESERVE_NAME_FIELDS.join(", "),
        found: fields
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vectors produced by the TypeScript implementation
    /// (`pop-dotns-testing/src/reservation.ts`, `buildReservationMessage`) on
    /// 2026-08-06. That implementation is itself cross-checked against the
    /// pallet and against `truapi`'s `build_reservation_message`.
    /// Independently derived bytes are the point. A shared helper could be
    /// wrong in both places.
    ///
    /// Inputs: candidate = `//dotns-golden` off the well-known dev mnemonic,
    /// attester = `0x01..0x20`, lite label `goldenname.42` (base `goldenname`),
    /// chat key `(3 * i) % 251` for `i` in `0..65`,
    /// `signed_at = 1_750_000_000`.
    const CANDIDATE: &str = "f68c00ac42085e1cc2624a084793d65f83be81149170f6a1460f2cdd3916fc4e";
    const MESSAGE_NONE: &str = "64706f703a646f746e732d676174657761793a72657365727665f68c00ac42085e1cc2624a084793d65f83be81149170f6a1460f2cdd3916fc4e0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2028676f6c64656e6e616d650501000306090c0f1215181b1e2124272a2d303336393c3f4245484b4e5154575a5d606366696c6f7275787b7e8184878a8d909396999c9fa2a5a8abaeb1b4b7babdc00080e14e6800000000";
    const SIGNATURE_NONE: &str = "f6ea04a6f4dcd733f0a092a9a56a95702a351542fc9ea98e8d0025a7e1babf1923c3cbe21d56065daf3321c4955fb01b4e507f4142375168ddf2d6792332e780";
    const MESSAGE_RESERVED: &str = "64706f703a646f746e732d676174657761793a72657365727665f68c00ac42085e1cc2624a084793d65f83be81149170f6a1460f2cdd3916fc4e0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2028676f6c64656e6e616d650501000306090c0f1215181b1e2124272a2d303336393c3f4245484b4e5154575a5d606366696c6f7275787b7e8184878a8d909396999c9fa2a5a8abaeb1b4b7babdc0013072657365727665646e616d6580e14e6800000000";
    const SIGNATURE_RESERVED: &str = "0099ade38865ebd0a1a2e866f6047dbdbf3d93476213b8ef1f7c30bf0593787b9d46ac9f944ba0a6bcb76db418d1386d89cee2197a13e6f01ab60545896e2287";

    const SIGNED_AT: u64 = 1_750_000_000;
    const RESERVED: &[u8] = b"reservedname";

    fn candidate() -> [u8; 32] {
        decode32(CANDIDATE)
    }

    fn attester() -> [u8; 32] {
        std::array::from_fn(|i| i as u8 + 1)
    }

    fn chat_key() -> Vec<u8> {
        (0..65u32).map(|i| ((i * 3) % 251) as u8).collect()
    }

    fn decode32(value: &str) -> [u8; 32] {
        hex::decode(value)
            .expect("golden vector is hex")
            .try_into()
            .expect("golden vector is 32 bytes")
    }

    fn message(reserved: Option<&[u8]>) -> Vec<u8> {
        reservation_message(
            &candidate(),
            &attester(),
            b"goldenname",
            &chat_key(),
            reserved,
            SIGNED_AT,
        )
    }

    /// The SCALE layout matches the independent TypeScript encoder byte for
    /// byte, in both the `None` and `Some` shapes of `reserved_base_label`.
    #[test]
    fn reservation_message_matches_the_golden_vectors() {
        assert_eq!(hex::encode(message(None)), MESSAGE_NONE);
        assert_eq!(hex::encode(message(Some(RESERVED))), MESSAGE_RESERVED);
    }

    /// Signatures produced over those messages by the candidate key verify here.
    #[test]
    fn golden_signatures_verify() {
        for (reserved, signature) in [(None, SIGNATURE_NONE), (Some(RESERVED), SIGNATURE_RESERVED)]
        {
            let signature = hex::decode(signature).expect("golden signature is hex");
            assert!(verify_reservation_signature(
                &signature,
                &candidate(),
                &attester(),
                b"goldenname",
                &chat_key(),
                reserved,
                SIGNED_AT,
            ));
        }
    }

    /// Every field the message commits to is load-bearing. Change one and the
    /// signature stops verifying. These are the failure modes that would
    /// otherwise only surface as an on-chain `InvalidAttestationSignature`.
    #[test]
    fn every_committed_field_is_load_bearing() {
        let signature = hex::decode(SIGNATURE_NONE).expect("golden signature is hex");
        let good = candidate();
        let attester = attester();
        let chat_key = chat_key();

        let verify = |attester: &[u8; 32], base: &[u8], reserved, signed_at| {
            verify_reservation_signature(
                &signature, &good, attester, base, &chat_key, reserved, signed_at,
            )
        };

        assert!(verify(&attester, b"goldenname", None, SIGNED_AT), "control");
        // A different attester. The exact break when ATTESTER_ACCOUNT and the
        // extrinsic's signed origin drift apart.
        assert!(!verify(&[9u8; 32], b"goldenname", None, SIGNED_AT));
        // The full label instead of its base.
        assert!(!verify(&attester, b"goldenname.42", None, SIGNED_AT));
        // reservedUsername present when the client signed without it.
        assert!(!verify(&attester, b"goldenname", Some(RESERVED), SIGNED_AT));
        // A tampered timestamp.
        assert!(!verify(&attester, b"goldenname", None, SIGNED_AT + 1));
        // A signature that is not 64 bytes is rejected, not panicked on.
        assert!(!verify_reservation_signature(
            &[1u8; 63],
            &good,
            &attester,
            b"goldenname",
            &chat_key,
            None,
            SIGNED_AT,
        ));
    }

    #[test]
    fn lite_base_stops_at_the_first_dot() {
        assert_eq!(lite_base("alice.42"), "alice");
        assert_eq!(lite_base("alice"), "alice");
        assert_eq!(lite_base("alice.team.07"), "alice");
    }

    /// The window is inclusive of its final second, matching the pallet's
    /// `now <= signed_at + MaxValiditySeconds`.
    #[test]
    fn expiry_matches_the_pallets_inclusive_bound() {
        // PreviewNet / Paseo next Asset Hub `MaxValiditySeconds`.
        const WINDOW: u64 = 259_200;
        assert!(!reservation_expired(1_000, WINDOW, 1_000));
        assert!(!reservation_expired(1_000, WINDOW, 1_000 + WINDOW as i64));
        assert!(reservation_expired(1_000, WINDOW, 1_001 + WINDOW as i64));
        // A far-future signed_at is not "expired". That is the skew check's job.
        assert!(!reservation_expired(i64::MAX, WINDOW, 1_000));
    }

    /// The PreviewNet shape is accepted. The Paseo next shape is rejected, with
    /// both lists named in the error. Field lists captured 2026-08-06.
    #[test]
    fn shape_guard_separates_the_two_asset_hub_runtimes() {
        assert!(check_reserve_name_shape(&RESERVE_NAME_FIELDS).is_ok());

        let paseo_next = [
            "candidate",
            "candidate_signature",
            "ring_vrf_key",
            "proof_of_ownership",
            "lite_label",
            "chat_key",
            "reserved_base_label",
        ];
        let error = check_reserve_name_shape(&paseo_next).expect_err("old shape must be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("proof_of_ownership"), "{rendered}");
        assert!(rendered.contains("signed_at"), "{rendered}");

        // Same arity, one argument renamed. Still a mismatch.
        let renamed = [
            "candidate",
            "candidate_signature",
            "lite_label",
            "chat_key",
            "reserved_label",
            "signed_at",
        ];
        assert!(check_reserve_name_shape(&renamed).is_err());

        // Same names, reordered. Still a mismatch.
        let reordered = [
            "candidate",
            "candidate_signature",
            "lite_label",
            "chat_key",
            "signed_at",
            "reserved_base_label",
        ];
        assert!(check_reserve_name_shape(&reordered).is_err());
    }
}
