//! `POST /api/v1/usernames` — the username write path (registration intake).
//!
//! JWT-gated + rate-limited. Validates the request, verifies the
//! `candidateSignature` proves control of the beneficiary account, selects the
//! discriminator, and persists a `RESERVED` row in the reservation outbox before
//! returning `202`. The `identity-chain-writer` binary drains the outbox onto People
//! Chain; the app confirms the result on-chain (`UsernameOwnerOf`), so there is
//! no backend status-poll route.
//!
//! Byte-compatible with the shipping iOS/Android `UsernameApi.register`: flat
//! request fields, `202 {base_username, digits, username, device_check_available?}`.

use std::collections::BTreeSet;
use std::str::FromStr as _;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rand::rngs::OsRng;
use rand::seq::SliceRandom as _;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::chain::outbox::{self, InsertError, NewReservation};
use crate::http::error::{AppError, AppResult};
use crate::http::extract::AuthAccount;
use crate::http::state::AppState;

use super::{available_digits, is_valid_base};

/// The flat registration request, matching the shipping clients verbatim.
#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// SS58 beneficiary account that will own the username.
    #[serde(rename = "candidateAccountId")]
    #[schema(
        rename = "candidateAccountId",
        example = "SS58 account matching JWT subject"
    )]
    candidate_account_id: String,
    /// Base username (lowercase ASCII, >= 6 chars).
    #[schema(example = "tallesx")]
    username: String,
    /// Optional two-digit suffix, e.g. `"07"`; random-free if omitted.
    #[serde(rename = "preferredDigits")]
    #[schema(rename = "preferredDigits", example = "07")]
    preferred_digits: Option<String>,
    /// `0x`-hex 64-byte sr25519 signature proving control of the candidate.
    #[serde(rename = "candidateSignature")]
    #[schema(rename = "candidateSignature", example = "0x...64 bytes...")]
    candidate_signature: String,
    /// `0x`-hex ring VRF key.
    #[serde(rename = "ringVrfKey")]
    #[schema(rename = "ringVrfKey", example = "0x...")]
    ring_vrf_key: String,
    /// `0x`-hex 64-byte ownership proof.
    #[serde(rename = "proofOfOwnership")]
    #[schema(rename = "proofOfOwnership", example = "0x...64 bytes...")]
    proof_of_ownership: String,
    /// `0x`-hex 64-byte consumer registration signature.
    #[serde(rename = "consumerRegistrationSignature")]
    #[schema(rename = "consumerRegistrationSignature", example = "0x...64 bytes...")]
    consumer_registration_signature: String,
    /// `0x`-hex 65-byte identifier key.
    #[serde(rename = "identifierKey")]
    #[schema(rename = "identifierKey", example = "0x...65 bytes...")]
    identifier_key: String,
    /// Optional DotNS reservation block.
    dotns: Option<Dotns>,
}

/// Optional DotNS reservation, parking a base label for later full-person use.
#[derive(Deserialize, ToSchema)]
pub(crate) struct Dotns {
    /// `0x`-hex 64-byte signature.
    #[schema(example = "0x...64 bytes...")]
    signature: String,
    /// Unix timestamp the reservation was signed at.
    #[serde(rename = "signedAt")]
    #[schema(rename = "signedAt", example = 1780000000i64)]
    signed_at: i64,
    /// Optional reserved-name override.
    #[serde(rename = "reservedUsername")]
    #[schema(rename = "reservedUsername", example = "optional-name")]
    reserved_username: Option<String>,
}

/// `202` body: the reserved full username and its parts.
#[derive(Serialize, ToSchema)]
pub struct RegisterResponse {
    /// Reserved base username.
    #[schema(example = "tallesx")]
    base_username: String,
    /// Selected two-digit discriminator.
    #[schema(example = "07")]
    digits: String,
    /// Full `base.NN` username.
    #[schema(example = "tallesx.07")]
    username: String,
    /// Present only when the DeviceCheck free-slot gate applies (Slice 6).
    #[serde(
        rename = "device_check_available",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(rename = "device_check_available")]
    device_check_available: Option<bool>,
}

/// Reserve a lite username and enqueue it for on-chain registration.
#[utoipa::path(
    post,
    path = "/api/v1/usernames",
    tag = "Usernames",
    security(("bearer_jwt" = [])),
    request_body = RegisterRequest,
    responses(
        (status = 202, description = "Reservation accepted into the outbox for on-chain registration.",
         body = RegisterResponse,
         example = json!({ "base_username": "tallesx", "digits": "07", "username": "tallesx.07" })),
        (status = 400, description = "Invalid username or malformed proof/hex field.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "WRONG_DATA", "message": "username must be at least 6 lowercase letters" })),
        (status = 401, description = "Missing or invalid bearer token.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "UNAUTHORIZED", "message": "unauthorized" })),
        (status = 403, description = "candidateSignature does not prove control of candidateAccountId.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "FORBIDDEN", "message": "candidateSignature does not prove control of candidateAccountId" })),
        (status = 409, description = "Preferred digits or username already taken.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "CONFLICT", "message": "username is already taken" }))
    )
)]
pub async fn register(
    State(state): State<AppState>,
    account: AuthAccount,
    Json(req): Json<RegisterRequest>,
) -> AppResult<(StatusCode, Json<RegisterResponse>)> {
    let base = req.username.trim().to_string();
    if !is_valid_base(&base) {
        return Err(AppError::bad_request(
            "username must be at least 6 lowercase letters",
        ));
    }

    // The beneficiary (candidate) account that will own the username.
    let candidate = subxt::utils::AccountId32::from_str(&req.candidate_account_id)
        .map_err(|_| AppError::bad_request("candidateAccountId is not a valid SS58 address"))?;

    // Relayed verbatim into the on-chain call; validate shape/length here.
    let candidate_signature = decode_hex("candidateSignature", &req.candidate_signature, Some(64))?;
    let ring_vrf_key = decode_hex("ringVrfKey", &req.ring_vrf_key, None)?;
    let proof_of_ownership = decode_hex("proofOfOwnership", &req.proof_of_ownership, Some(64))?;
    let consumer_registration_signature = decode_hex(
        "consumerRegistrationSignature",
        &req.consumer_registration_signature,
        Some(64),
    )?;
    let identifier_key = decode_hex("identifierKey", &req.identifier_key, Some(65))?;

    // Authorization: prove the caller controls the beneficiary account. The
    // candidate key must have signed `pop:people-lite:register using ||
    // candidatePubkey || ringVrfKey` — the same message the People Chain
    // reconstructs for `PeopleLite.attest`. The JWT authenticates only the
    // device/session (a separate per-install auth key), so the beneficiary is
    // authorized by this signature, not by the JWT subject.
    if !verify_candidate_signature(&candidate.0, &candidate_signature, &ring_vrf_key) {
        return Err(AppError::forbidden(
            "candidateSignature does not prove control of candidateAccountId",
        ));
    }

    let (dotns_signature, dotns_signed_at, reserved_username) = match req.dotns {
        Some(d) => (
            Some(decode_hex("dotns.signature", &d.signature, Some(64))?),
            Some(d.signed_at),
            d.reserved_username,
        ),
        None => (None, None, None),
    };

    // Attestation-off (M0): the iOS DeviceCheck free-slot gate that would return
    // `200 {registrationOutcome: "PAYMENT_REQUIRED"}` is wired in Slice 6. While
    // `auth_enabled` is false every request is treated as an instant free reservation.
    if state.config.auth_enabled {
        tracing::debug!("attestation enabled: DeviceCheck free-slot gate not yet implemented");
    }

    let taken = state.chain.taken_discriminators(&base).await?;
    let digit = select_digit(&taken, req.preferred_digits.as_deref())?;
    let digits = format!("{digit:02}");
    let full_username = format!("{base}.{digits}");

    let new = NewReservation {
        account_id: account.account_id.clone(),
        candidate_account_id: req.candidate_account_id,
        base: base.clone(),
        digits: digits.clone(),
        full_username: full_username.clone(),
        candidate_signature,
        ring_vrf_key,
        proof_of_ownership,
        consumer_registration_signature,
        identifier_key,
        dotns_signature,
        dotns_signed_at,
        reserved_username,
    };

    match outbox::insert(&state.pool, &new).await {
        Ok(id) => {
            tracing::info!(id, username = %full_username, "username reserved");
        }
        Err(InsertError::Conflict) => {
            return Err(AppError::conflict("username is already taken"));
        }
        Err(InsertError::Db(e)) => return Err(AppError::from(e)),
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(RegisterResponse {
            base_username: base,
            digits,
            username: full_username,
            device_check_available: None,
        }),
    ))
}

/// Decode a `0x`-hex field, optionally enforcing an exact byte length.
fn decode_hex(name: &str, value: &str, expect_len: Option<usize>) -> AppResult<Vec<u8>> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|_| AppError::bad_request(format!("{name} must be 0x-hex")))?;
    if let Some(n) = expect_len {
        if bytes.len() != n {
            return Err(AppError::bad_request(format!(
                "{name} must be {n} bytes, got {}",
                bytes.len()
            )));
        }
    }
    Ok(bytes)
}

/// The message prefix the People Chain reconstructs for `PeopleLite.attest`;
/// `candidateSignature` (and `proofOfOwnership`) cover
/// `POP_REGISTER_PREFIX || candidatePubkey || ringVrfKey`.
const POP_REGISTER_PREFIX: &[u8] = b"pop:people-lite:register using";

/// Verify the caller controls `candidate_pubkey`: the candidate's sr25519 key
/// must have signed `POP_REGISTER_PREFIX || candidatePubkey || ringVrfKey`. Uses
/// `subxt_signer` (schnorrkel with the Substrate signing context), symmetric
/// with the shipping Polkadot wallets and the on-chain verifier.
fn verify_candidate_signature(
    candidate_pubkey: &[u8; 32],
    candidate_signature: &[u8],
    ring_vrf_key: &[u8],
) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(candidate_signature) else {
        return false;
    };
    let mut message = Vec::with_capacity(POP_REGISTER_PREFIX.len() + 32 + ring_vrf_key.len());
    message.extend_from_slice(POP_REGISTER_PREFIX);
    message.extend_from_slice(candidate_pubkey);
    message.extend_from_slice(ring_vrf_key);
    subxt_signer::sr25519::verify(
        &subxt_signer::sr25519::Signature(signature),
        message,
        &subxt_signer::sr25519::PublicKey(*candidate_pubkey),
    )
}

/// Choose the discriminator: the preferred one if free, else a random free one.
/// The pool is `01..=99` (`00` is never offered).
fn select_digit(taken: &BTreeSet<u8>, preferred: Option<&str>) -> AppResult<u8> {
    match preferred {
        Some(p) => {
            let d = parse_preferred(p)
                .ok_or_else(|| AppError::bad_request("preferredDigits must be 01-99"))?;
            if taken.contains(&d) {
                return Err(AppError::conflict("preferred digits are taken"));
            }
            Ok(d)
        }
        None => {
            let available = available_digits(taken);
            available
                .choose(&mut OsRng)
                .copied()
                .ok_or_else(|| AppError::conflict("no digits available for this name"))
        }
    }
}

/// Parse a two-character `01`..`99` suffix into `1..=99`.
fn parse_preferred(s: &str) -> Option<u8> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match s.parse::<u8>().ok()? {
        d @ 1..=99 => Some(d),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registration_contract_matches_ios_and_android() {
        let request = serde_json::from_value::<RegisterRequest>(json!({
            "candidateAccountId": "5DypZvug3B3fCu2EJhLjXCpJGasDcNqSoAHXbxEPAQyDavGj",
            "username": "aliceuser",
            "preferredDigits": "42",
            "candidateSignature": format!("0x{}", "01".repeat(64)),
            "ringVrfKey": "0x0203",
            "proofOfOwnership": format!("0x{}", "03".repeat(64)),
            "consumerRegistrationSignature": format!("0x{}", "04".repeat(64)),
            "identifierKey": format!("0x{}", "05".repeat(65))
        }))
        .unwrap();
        assert_eq!(request.username, "aliceuser");
        assert_eq!(request.preferred_digits.as_deref(), Some("42"));
        assert!(request.dotns.is_none());

        let response = RegisterResponse {
            base_username: "aliceuser".to_string(),
            digits: "42".to_string(),
            username: "aliceuser.42".to_string(),
            device_check_available: None,
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "base_username": "aliceuser",
                "digits": "42",
                "username": "aliceuser.42"
            })
        );
    }

    #[test]
    fn registration_contract_accepts_optional_dotns_fields() {
        let request = serde_json::from_value::<RegisterRequest>(json!({
            "candidateAccountId": "5DypZvug3B3fCu2EJhLjXCpJGasDcNqSoAHXbxEPAQyDavGj",
            "username": "aliceuser",
            "candidateSignature": format!("0x{}", "01".repeat(64)),
            "ringVrfKey": "0x0203",
            "proofOfOwnership": format!("0x{}", "03".repeat(64)),
            "consumerRegistrationSignature": format!("0x{}", "04".repeat(64)),
            "identifierKey": format!("0x{}", "05".repeat(65)),
            "dotns": {
                "signature": format!("0x{}", "06".repeat(64)),
                "signedAt": 123,
                "reservedUsername": "alice"
            }
        }))
        .unwrap();

        assert!(request.preferred_digits.is_none());
        let dotns = request.dotns.unwrap();
        assert_eq!(dotns.signed_at, 123);
        assert_eq!(dotns.reserved_username.as_deref(), Some("alice"));
    }

    #[test]
    fn base_validation_matches_read_path() {
        assert!(is_valid_base("alicexyz"));
        assert!(!is_valid_base("short")); // < 6
        assert!(!is_valid_base("Alicexyz")); // uppercase
        assert!(!is_valid_base("alice123")); // digits
    }

    #[test]
    fn parses_two_digit_preferred_suffix() {
        assert_eq!(parse_preferred("01"), Some(1));
        assert_eq!(parse_preferred("42"), Some(42));
        assert_eq!(parse_preferred("99"), Some(99));
        assert_eq!(parse_preferred("00"), None); // 00 never offered
        assert_eq!(parse_preferred("5"), None); // must be two chars
        assert_eq!(parse_preferred("100"), None);
        assert_eq!(parse_preferred("ab"), None);
    }

    #[test]
    fn available_excludes_taken_and_zero() {
        let taken: BTreeSet<u8> = [1u8, 2, 99].into_iter().collect();
        let available = available_digits(&taken);
        assert!(!available.contains(&0));
        assert!(!available.contains(&1));
        assert!(!available.contains(&99));
        assert!(available.contains(&3));
        assert_eq!(available.len(), 99 - 3);
    }

    #[test]
    fn preferred_digit_selection() {
        let taken: BTreeSet<u8> = [7u8].into_iter().collect();
        assert_eq!(select_digit(&taken, Some("08")).unwrap(), 8);
        assert!(matches!(
            select_digit(&taken, Some("07")),
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            select_digit(&taken, Some("zz")),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn random_selection_when_exhausted_conflicts() {
        let taken: BTreeSet<u8> = (1..=99u8).collect();
        assert!(matches!(
            select_digit(&taken, None),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn candidate_signature_verifies_control_of_beneficiary() {
        use std::str::FromStr as _;
        use subxt_signer::sr25519::Keypair;
        use subxt_signer::SecretUri;

        let keypair = Keypair::from_uri(&SecretUri::from_str("//Alice").unwrap()).unwrap();
        let candidate = keypair.public_key().0;
        let ring_vrf_key = [7u8; 32];

        let mut message = Vec::new();
        message.extend_from_slice(POP_REGISTER_PREFIX);
        message.extend_from_slice(&candidate);
        message.extend_from_slice(&ring_vrf_key);
        let signature = keypair.sign(&message).0;

        // A valid signature by the candidate over the exact message passes.
        assert!(verify_candidate_signature(
            &candidate,
            &signature,
            &ring_vrf_key
        ));
        // Tampering with the ring key (bound into the message) fails.
        assert!(!verify_candidate_signature(
            &candidate, &signature, &[9u8; 32]
        ));
        // A different account cannot be the beneficiary of this signature.
        let other = Keypair::from_uri(&SecretUri::from_str("//Bob").unwrap())
            .unwrap()
            .public_key()
            .0;
        assert!(!verify_candidate_signature(
            &other,
            &signature,
            &ring_vrf_key
        ));
        // A malformed (wrong-length) signature is rejected, not panicked.
        assert!(!verify_candidate_signature(
            &candidate,
            &[0u8; 10],
            &ring_vrf_key
        ));
    }
}
