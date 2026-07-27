//! `POST /api/v1/usernames` — the username write path (registration intake).
//!
//! JWT-gated (the frozen 401 trio via `AuthSubject`) + subject-rate-limited.
//! Validates the request against the golden-captured legacy zod dialect,
//! verifies the `candidateSignature` proves control of the beneficiary
//! account, selects the discriminator, and persists a `RESERVED` row in the
//! reservation outbox before returning `202`. The `identity-chain-writer`
//! binary drains the outbox onto People Chain.
//!
//! Validation keeps the legacy three-phase zod semantics
//! (`tests/fixtures/legacy-wire.json`):
//! 1. **Type errors** (missing/wrong-typed fields) collect in field order;
//!    when any exist, the cross-field refinement never runs.
//! 2. **Check errors** (regex failures) also collect in field order, but the
//!    legacy `abort: true` fields (`username`, `ringVrfKey`,
//!    `dotns.reservedUsername`) additionally suppress the refinement.
//! 3. **The refinement** (ss58 validity → signature verification → base
//!    length → dotns gating/freshness) appends its issues after the field
//!    errors. The legacy quirk where a regex-failed `candidateSignature`
//!    reached the wasm sr25519 verifier and threw an opaque 500 was retired
//!    2026-07-23: the pattern failure is a normal 400 field error and the
//!    verifier is simply skipped.

use std::collections::BTreeSet;
use std::str::FromStr as _;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use http_common::{zod, AuthSubject};
use rand::rngs::OsRng;
use rand::seq::SliceRandom as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::chain::outbox::{self, InsertError, NewReservation};
use crate::http::state::AppState;

use super::error::{FieldError, UsernamesError, UsernamesResult};
use super::{available_digits, taken_discriminators, MAX_BASE_LEN};

/// The flat registration request (documentation mirror — the handler
/// validates raw JSON to reproduce the legacy zod error bodies).
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct RegisterRequest {
    /// SS58 beneficiary account that will own the username.
    #[serde(rename = "candidateAccountId")]
    #[schema(
        rename = "candidateAccountId",
        example = "5FbRAkhDvNVecNzHLFxBNXFXNwvBaV69S1W3nfBbnxYypkkT"
    )]
    candidate_account_id: String,
    /// Base username (lowercase ASCII letters, 6..=29 chars).
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
#[allow(dead_code)]
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
    #[schema(rename = "reservedUsername", example = "reservedname")]
    reserved_username: Option<String>,
}

/// `202` body: the reserved full username and its parts.
#[derive(Serialize, ToSchema)]
pub struct RegisterResponse {
    /// Reserved base username.
    #[schema(example = "tallesx")]
    pub base_username: String,
    /// Selected two-digit discriminator.
    #[schema(example = "07")]
    pub digits: String,
    /// Full `base.NN` username.
    #[schema(example = "tallesx.07")]
    pub username: String,
    /// Present only when the DeviceCheck free-slot gate applies (the
    /// attestation slice; deferred).
    #[serde(
        rename = "device_check_available",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(rename = "device_check_available")]
    pub device_check_available: Option<bool>,
}

/// Captured legacy zod messages (field checks + refinement issues).
const MSG_HEX_64: &str = "Must be a hexadecimal string of exactly 64 bytes.";
const MSG_HEX_65: &str = "Must be a hexadecimal string of exactly 65 bytes.";
const MSG_DIGITS: &str = "Digits must be between 01-99";
const MSG_INVALID_SS58: &str = "Invalid ss58 address.";
const MSG_INVALID_SIGNATURE: &str = "Invalid signature.";
const MSG_DOTNS_DISABLED: &str = "dotNS gateway is not enabled in this environment.";
const PATTERN_BASE: &str = "/^([a-z]{6,})$/";
const PATTERN_HEX_STRING: &str = "/^(0x)?[a-fA-F0-9]+$/";
const DEVICE_TOKEN_HEADER: &str = "Device-Token-iOS";

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
        (status = 400, description = "Validation failed (problem details with `errors`) or malformed JSON.",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-body-property-value",
             "title": "Invalid Body Property Value",
             "detail": "The request body contains an invalid body property value.",
             "status": 400,
             "errors": [{ "detail": "Invalid signature.", "pointer": "#/candidateSignature" }]
         })),
        (status = 401, description = "Missing or invalid bearer token (the frozen problem-details trio).",
         body = serde_json::Value),
        (status = 409, description = "Preferred digits taken, no digits available, or username taken.",
         body = serde_json::Value,
         example = json!({ "error": "Preferred digits 07 already taken for username tallesx" })),
        (status = 429, description = "Subject rate limit exceeded (problem details with `Retry-After`).",
         body = serde_json::Value),
        (status = 500, description = "Persistence failure or unexpected error.",
         body = serde_json::Value,
         example = json!({ "error": "Failed to persist username registration" }))
    )
)]
pub async fn register(
    State(state): State<AppState>,
    auth: AuthSubject,
    headers: HeaderMap,
    body: Bytes,
) -> UsernamesResult<(StatusCode, Json<RegisterResponse>)> {
    super::check_rate_limit(&state, &auth.subject)?;
    validate_device_token_header(&headers)?;

    let value = super::parse_json_body(&body)?;
    let parsed = validate_register(&value, &state.config)?;

    let taken = taken_discriminators(&state, &parsed.username).await?;
    let digit = select_digit(&taken, parsed.preferred_digits.as_deref(), &parsed.username)?;
    let digits = format!("{digit:02}");
    let full_username = format!("{}.{digits}", parsed.username);

    let new = NewReservation {
        account_id: auth.subject.clone(),
        candidate_account_id: parsed.candidate_account_id,
        base: parsed.username.clone(),
        digits: digits.clone(),
        full_username: full_username.clone(),
        candidate_signature: parsed.candidate_signature,
        ring_vrf_key: parsed.ring_vrf_key,
        proof_of_ownership: parsed.proof_of_ownership,
        consumer_registration_signature: parsed.consumer_registration_signature,
        identifier_key: parsed.identifier_key,
        dotns_signature: parsed.dotns.as_ref().map(|d| d.signature.clone()),
        dotns_signed_at: parsed.dotns.as_ref().map(|d| d.signed_at),
        reserved_username: parsed.dotns.and_then(|d| d.reserved_username),
    };

    match outbox::insert(&state.pool, &new).await {
        Ok(id) => {
            tracing::info!(id, username = %full_username, "username reserved");
        }
        Err(InsertError::Conflict) => {
            return Err(UsernamesError::UsernameTaken {
                base: parsed.username,
                digits,
            });
        }
        Err(InsertError::Db(e)) => {
            tracing::error!(error = ?e, "reservation outbox insert failed");
            return Err(UsernamesError::PersistenceFailed);
        }
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(RegisterResponse {
            base_username: parsed.username,
            digits,
            username: full_username,
            device_check_available: None,
        }),
    ))
}

/// The fully-validated registration request, with hex fields decoded.
struct ParsedRegister {
    candidate_account_id: String,
    username: String,
    preferred_digits: Option<String>,
    candidate_signature: Vec<u8>,
    ring_vrf_key: Vec<u8>,
    proof_of_ownership: Vec<u8>,
    consumer_registration_signature: Vec<u8>,
    identifier_key: Vec<u8>,
    dotns: Option<ParsedDotns>,
}

struct ParsedDotns {
    signature: Vec<u8>,
    signed_at: i64,
    reserved_username: Option<String>,
}

/// Validate `Device-Token-iOS` per the legacy header schema
/// (`z.string().base64()` — the captured header dialect).
fn validate_device_token_header(headers: &HeaderMap) -> UsernamesResult<()> {
    let Some(raw) = headers.get(DEVICE_TOKEN_HEADER) else {
        return Ok(());
    };
    let valid = raw.to_str().is_ok_and(is_zod_base64);
    if valid {
        return Ok(());
    }
    Err(UsernamesError::InvalidHeader(vec![FieldError {
        detail: "Invalid base64-encoded string".to_string(),
        pointer: format!("#/{DEVICE_TOKEN_HEADER}"),
    }]))
}

/// The zod v4 base64 rule: empty, or padded groups of four.
fn is_zod_base64(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if !value.len().is_multiple_of(4) {
        return false;
    }
    let core = value.trim_end_matches('=');
    value.len() - core.len() <= 2
        && core
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Validate the raw body against the frozen legacy dialect (see module docs
/// for the three-phase semantics).
fn validate_register(
    value: &Value,
    config: &crate::config::Config,
) -> Result<ParsedRegister, UsernamesError> {
    let mut errors: Vec<FieldError> = Vec::new();
    let object = value.as_object();

    let get = |field: &str| object.and_then(|o| o.get(field));

    // A required string field: missing / non-string is a type error.
    let required_string = |errors: &mut Vec<FieldError>, field: &str| -> Option<String> {
        match get(field) {
            Some(Value::String(s)) => Some(s.clone()),
            other => {
                errors.push(FieldError {
                    detail: zod::invalid_input("string", zod::received_name(other)),
                    pointer: format!("#/{field}"),
                });
                None
            }
        }
    };

    let mut refine_skipped = false;

    // --- field phase, in zod field order --------------------------------
    let candidate_account_id = required_string(&mut errors, "candidateAccountId");
    if candidate_account_id.is_none() {
        refine_skipped = true;
    }

    let username = required_string(&mut errors, "username");
    match &username {
        None => refine_skipped = true,
        Some(name) => {
            if !(name.len() >= 6 && name.bytes().all(|b| b.is_ascii_lowercase())) {
                errors.push(FieldError {
                    detail: zod::invalid_string_pattern(PATTERN_BASE),
                    pointer: "#/username".to_string(),
                });
                refine_skipped = true;
            }
        }
    }

    let preferred_digits = match get("preferredDigits") {
        None => None,
        Some(Value::String(s)) => {
            if !is_preferred_digits(s) {
                errors.push(FieldError {
                    detail: MSG_DIGITS.to_string(),
                    pointer: "#/preferredDigits".to_string(),
                });
            }
            Some(s.clone())
        }
        Some(other) => {
            errors.push(FieldError {
                detail: zod::invalid_input("string", zod::type_name(other)),
                pointer: "#/preferredDigits".to_string(),
            });
            refine_skipped = true;
            None
        }
    };

    // `0x`-prefixed exact-length hex fields (no abort): a failed check still
    // lets the refinement run, which is where the legacy 500 quirk lives.
    let fixed_hex = |errors: &mut Vec<FieldError>,
                     refine_skipped: &mut bool,
                     field: &str,
                     hex_digits: usize,
                     message: &str|
     -> (Option<String>, bool) {
        match get(field) {
            Some(Value::String(s)) => {
                let ok = is_prefixed_hex(s, hex_digits);
                if !ok {
                    errors.push(FieldError {
                        detail: message.to_string(),
                        pointer: format!("#/{field}"),
                    });
                }
                (Some(s.clone()), ok)
            }
            other => {
                errors.push(FieldError {
                    detail: zod::invalid_input("string", zod::received_name(other)),
                    pointer: format!("#/{field}"),
                });
                *refine_skipped = true;
                (None, false)
            }
        }
    };

    let (candidate_signature_raw, candidate_signature_ok) = fixed_hex(
        &mut errors,
        &mut refine_skipped,
        "candidateSignature",
        128,
        MSG_HEX_64,
    );

    let ring_vrf_key = match get("ringVrfKey") {
        Some(Value::String(s)) => {
            let stripped = s.strip_prefix("0x").unwrap_or(s);
            if stripped.is_empty() || !stripped.bytes().all(|b| b.is_ascii_hexdigit()) {
                errors.push(FieldError {
                    detail: zod::invalid_string_pattern(PATTERN_HEX_STRING),
                    pointer: "#/ringVrfKey".to_string(),
                });
                refine_skipped = true;
                None
            } else {
                Some(s.clone())
            }
        }
        other => {
            errors.push(FieldError {
                detail: zod::invalid_input("string", zod::received_name(other)),
                pointer: "#/ringVrfKey".to_string(),
            });
            refine_skipped = true;
            None
        }
    };

    let (proof_of_ownership_raw, _) = fixed_hex(
        &mut errors,
        &mut refine_skipped,
        "proofOfOwnership",
        128,
        MSG_HEX_64,
    );
    let (consumer_registration_signature_raw, _) = fixed_hex(
        &mut errors,
        &mut refine_skipped,
        "consumerRegistrationSignature",
        128,
        MSG_HEX_64,
    );
    let (identifier_key_raw, _) = fixed_hex(
        &mut errors,
        &mut refine_skipped,
        "identifierKey",
        130,
        MSG_HEX_65,
    );

    let dotns = match get("dotns") {
        None => None,
        Some(Value::Object(block)) => {
            let mut signature = None;
            match block.get("signature") {
                Some(Value::String(s)) => {
                    if is_prefixed_hex(s, 128) {
                        signature = Some(s.clone());
                    } else {
                        errors.push(FieldError {
                            detail: MSG_HEX_64.to_string(),
                            pointer: "#/dotns/signature".to_string(),
                        });
                    }
                }
                other => {
                    errors.push(FieldError {
                        detail: zod::invalid_input("string", zod::received_name(other)),
                        pointer: "#/dotns/signature".to_string(),
                    });
                    refine_skipped = true;
                }
            }
            let signed_at = match block.get("signedAt") {
                Some(Value::Number(n)) => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && f >= 0.0 => Some(f as i64),
                    Some(f) if f.fract() != 0.0 => {
                        errors.push(FieldError {
                            detail: zod::invalid_input("int", "number"),
                            pointer: "#/dotns/signedAt".to_string(),
                        });
                        refine_skipped = true;
                        None
                    }
                    _ => {
                        errors.push(FieldError {
                            detail: "Too small: expected number to be >=0".to_string(),
                            pointer: "#/dotns/signedAt".to_string(),
                        });
                        refine_skipped = true;
                        None
                    }
                },
                other => {
                    errors.push(FieldError {
                        detail: zod::invalid_input("number", zod::received_name(other)),
                        pointer: "#/dotns/signedAt".to_string(),
                    });
                    refine_skipped = true;
                    None
                }
            };
            let reserved_username = match block.get("reservedUsername") {
                None => None,
                Some(Value::String(s)) => {
                    if s.len() >= 6 && s.bytes().all(|b| b.is_ascii_lowercase()) {
                        Some(s.clone())
                    } else {
                        errors.push(FieldError {
                            detail: zod::invalid_string_pattern(PATTERN_BASE),
                            pointer: "#/dotns/reservedUsername".to_string(),
                        });
                        refine_skipped = true;
                        None
                    }
                }
                Some(other) => {
                    errors.push(FieldError {
                        detail: zod::invalid_input("string", zod::type_name(other)),
                        pointer: "#/dotns/reservedUsername".to_string(),
                    });
                    refine_skipped = true;
                    None
                }
            };
            Some((signature, signed_at, reserved_username))
        }
        Some(other) => {
            errors.push(FieldError {
                detail: zod::invalid_input("object", zod::type_name(other)),
                pointer: "#/dotns".to_string(),
            });
            refine_skipped = true;
            None
        }
    };

    // --- refinement phase ----------------------------------------------
    if !refine_skipped {
        let account_id = candidate_account_id.as_deref().unwrap_or_default();
        match subxt::utils::AccountId32::from_str(account_id) {
            Err(_) => errors.push(FieldError {
                detail: MSG_INVALID_SS58.to_string(),
                pointer: "#/candidateAccountId".to_string(),
            }),
            Ok(candidate) => {
                // A regex-failed candidateSignature already carries its 400
                // field error from the check phase; the verifier is skipped.
                // (Legacy instead fed the raw string to the wasm verifier,
                // which threw an opaque 500 — quirk retired 2026-07-23.)
                if candidate_signature_ok {
                    let signature = decode_prefixed_hex(candidate_signature_raw.as_deref());
                    let ring = decode_prefixed_hex(ring_vrf_key.as_deref());
                    if !verify_candidate_signature(&candidate.0, &signature, &ring) {
                        errors.push(FieldError {
                            detail: MSG_INVALID_SIGNATURE.to_string(),
                            pointer: "#/candidateSignature".to_string(),
                        });
                    }
                }
            }
        }

        if let Some(name) = &username {
            if name.len() > MAX_BASE_LEN {
                errors.push(FieldError {
                    detail: format!("Username sum exceeds the maximum length: ({MAX_BASE_LEN})."),
                    pointer: "#/username".to_string(),
                });
            }
        }

        if let Some((_, signed_at, _)) = &dotns {
            if !config.dotns_gateway_enabled {
                errors.push(FieldError {
                    detail: MSG_DOTNS_DISABLED.to_string(),
                    pointer: "#/dotns".to_string(),
                });
            } else if let Some(signed_at) = signed_at {
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                let skew = config.dotns_max_future_skew_secs as i64;
                let max_age = config.dotns_intake_freshness_max_age_secs as i64;
                if *signed_at > now + skew {
                    errors.push(FieldError {
                        detail: format!("signedAt is in the future (tolerance {skew}s)."),
                        pointer: "#/dotns/signedAt".to_string(),
                    });
                }
                if now - signed_at > max_age {
                    errors.push(FieldError {
                        detail: format!(
                            "signedAt is older than the intake freshness bound ({max_age}s). \
                             Re-sign with a fresh timestamp and resubmit."
                        ),
                        pointer: "#/dotns/signedAt".to_string(),
                    });
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(UsernamesError::InvalidBody(errors));
    }

    Ok(ParsedRegister {
        candidate_account_id: candidate_account_id.unwrap_or_default(),
        username: username.unwrap_or_default(),
        preferred_digits,
        candidate_signature: decode_prefixed_hex(candidate_signature_raw.as_deref()),
        ring_vrf_key: decode_prefixed_hex(ring_vrf_key.as_deref()),
        proof_of_ownership: decode_prefixed_hex(proof_of_ownership_raw.as_deref()),
        consumer_registration_signature: decode_prefixed_hex(
            consumer_registration_signature_raw.as_deref(),
        ),
        identifier_key: decode_prefixed_hex(identifier_key_raw.as_deref()),
        dotns: dotns.map(|(signature, signed_at, reserved_username)| ParsedDotns {
            signature: decode_prefixed_hex(signature.as_deref()),
            signed_at: signed_at.unwrap_or_default(),
            reserved_username,
        }),
    })
}

/// `^0x[a-fA-F0-9]{n}$`.
fn is_prefixed_hex(value: &str, hex_digits: usize) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|rest| rest.len() == hex_digits && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `^(0[1-9]|[1-9][0-9])$`.
fn is_preferred_digits(value: &str) -> bool {
    matches!(
        value.as_bytes(),
        [b'0', b'1'..=b'9'] | [b'1'..=b'9', b'0'..=b'9']
    )
}

/// Decode a validated `0x`-optional hex string (empty on `None`).
fn decode_prefixed_hex(value: Option<&str>) -> Vec<u8> {
    let value = value.unwrap_or_default();
    hex::decode(value.strip_prefix("0x").unwrap_or(value)).unwrap_or_default()
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

/// Choose the discriminator: the preferred one if free, else a random free
/// one. The pool is `01..=99` (`00` is never offered). The preferred string
/// is regex-validated before this point.
fn select_digit(taken: &BTreeSet<u8>, preferred: Option<&str>, base: &str) -> UsernamesResult<u8> {
    match preferred {
        Some(p) => {
            let d: u8 = p.parse().map_err(|_| {
                UsernamesError::Internal(anyhow::anyhow!("preferredDigits validated upstream"))
            })?;
            if taken.contains(&d) {
                return Err(UsernamesError::PreferredDigitsTaken {
                    digits: p.to_string(),
                    base: base.to_string(),
                });
            }
            Ok(d)
        }
        None => {
            let available = available_digits(taken);
            available
                .choose(&mut OsRng)
                .copied()
                .ok_or_else(|| UsernamesError::NoDigitsAvailable {
                    base: base.to_string(),
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> crate::config::Config {
        crate::config::Config::test_default()
    }

    fn valid_body() -> serde_json::Value {
        use std::str::FromStr as _;
        use subxt_signer::sr25519::Keypair;
        use subxt_signer::SecretUri;

        let keypair = Keypair::from_uri(&SecretUri::from_str("//Alice").unwrap()).unwrap();
        let candidate = keypair.public_key();
        let ring = [7u8; 32];
        let mut message = Vec::new();
        message.extend_from_slice(POP_REGISTER_PREFIX);
        message.extend_from_slice(&candidate.0);
        message.extend_from_slice(&ring);
        let signature = keypair.sign(&message).0;

        json!({
            "candidateAccountId": candidate.to_account_id().to_string(),
            "username": "aliceuser",
            "candidateSignature": format!("0x{}", hex::encode(signature)),
            "ringVrfKey": format!("0x{}", hex::encode(ring)),
            "proofOfOwnership": format!("0x{}", "03".repeat(64)),
            "consumerRegistrationSignature": format!("0x{}", "04".repeat(64)),
            "identifierKey": format!("0x{}", "05".repeat(65))
        })
    }

    fn errors_of(value: serde_json::Value) -> Vec<(String, String)> {
        match validate_register(&value, &config()) {
            Err(UsernamesError::InvalidBody(errors)) => {
                errors.into_iter().map(|e| (e.pointer, e.detail)).collect()
            }
            other => panic!("expected InvalidBody, got {other:?}", other = other.err()),
        }
    }

    #[test]
    fn accepts_a_genuinely_signed_request() {
        let parsed = validate_register(&valid_body(), &config()).expect("valid");
        assert_eq!(parsed.username, "aliceuser");
        assert_eq!(parsed.candidate_signature.len(), 64);
    }

    #[test]
    fn empty_object_reports_every_required_field_in_order() {
        let pointers: Vec<String> = errors_of(json!({}))
            .into_iter()
            .map(|(pointer, _)| pointer)
            .collect();
        assert_eq!(
            pointers,
            [
                "#/candidateAccountId",
                "#/username",
                "#/candidateSignature",
                "#/ringVrfKey",
                "#/proofOfOwnership",
                "#/consumerRegistrationSignature",
                "#/identifierKey",
            ]
        );
    }

    #[test]
    fn wrong_signature_is_a_400_with_the_captured_message() {
        let mut body = valid_body();
        body["candidateSignature"] = json!(format!("0x{}", "ab".repeat(64)));
        assert_eq!(
            errors_of(body),
            [(
                "#/candidateSignature".to_string(),
                "Invalid signature.".to_string()
            )]
        );
    }

    #[test]
    fn regex_failed_signature_is_a_normal_400() {
        // Legacy 500'd here (the raw string reached the wasm verifier and
        // threw); the retired quirk is now the plain pattern field error.
        for bad in ["nothex", &format!("0x{}", "ab".repeat(63))] {
            let mut body = valid_body();
            body["candidateSignature"] = json!(bad);
            assert_eq!(
                errors_of(body),
                [("#/candidateSignature".to_string(), MSG_HEX_64.to_string())],
                "{bad}"
            );
        }
    }

    #[test]
    fn abort_fields_suppress_the_refinement() {
        // A bad ringVrfKey (abort) with a bad signature: only the pattern
        // message — no signature issue, no 500.
        let mut body = valid_body();
        body["ringVrfKey"] = json!("nothex!");
        assert_eq!(
            errors_of(body),
            [(
                "#/ringVrfKey".to_string(),
                zod::invalid_string_pattern(PATTERN_HEX_STRING)
            )]
        );

        let mut body = valid_body();
        body["username"] = json!("Abcdef");
        assert_eq!(
            errors_of(body),
            [(
                "#/username".to_string(),
                zod::invalid_string_pattern(PATTERN_BASE)
            )]
        );
    }

    #[test]
    fn type_errors_suppress_the_refinement() {
        // All-numbers body: only the seven type errors, no ss58 issue.
        let body = json!({
            "candidateAccountId": 5, "username": 6, "candidateSignature": 7,
            "ringVrfKey": 8, "proofOfOwnership": 9,
            "consumerRegistrationSignature": 10, "identifierKey": 11
        });
        let errors = errors_of(body);
        assert_eq!(errors.len(), 7);
        assert!(errors
            .iter()
            .all(|(_, detail)| detail == "Invalid input: expected string, received number"));
    }

    #[test]
    fn refinement_issues_use_the_captured_messages() {
        let mut body = valid_body();
        body["candidateAccountId"] = json!("not-an-address");
        assert_eq!(
            errors_of(body),
            [(
                "#/candidateAccountId".to_string(),
                "Invalid ss58 address.".to_string()
            )]
        );

        let mut body = valid_body();
        body["username"] = json!("a".repeat(30));
        assert_eq!(
            errors_of(body),
            [(
                "#/username".to_string(),
                "Username sum exceeds the maximum length: (29).".to_string()
            )]
        );
    }

    #[test]
    fn preferred_digits_and_identifier_checks_collect_in_field_order() {
        let mut body = valid_body();
        body["preferredDigits"] = json!("00");
        body["identifierKey"] = json!(format!("0x{}", "ab".repeat(64)));
        assert_eq!(
            errors_of(body),
            [
                (
                    "#/preferredDigits".to_string(),
                    "Digits must be between 01-99".to_string()
                ),
                (
                    "#/identifierKey".to_string(),
                    "Must be a hexadecimal string of exactly 65 bytes.".to_string()
                ),
            ]
        );
    }

    #[test]
    fn dotns_gating_and_freshness_use_the_captured_messages() {
        let mut disabled = config();
        disabled.dotns_gateway_enabled = false;
        let mut body = valid_body();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        body["dotns"] = json!({ "signature": format!("0x{}", "ab".repeat(64)), "signedAt": now });
        match validate_register(&body, &disabled) {
            Err(UsernamesError::InvalidBody(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].pointer, "#/dotns");
                assert_eq!(errors[0].detail, MSG_DOTNS_DISABLED);
            }
            other => panic!("expected InvalidBody, got {other:?}", other = other.err()),
        }

        let enabled = config();
        let mut future = valid_body();
        future["dotns"] =
            json!({ "signature": format!("0x{}", "ab".repeat(64)), "signedAt": now + 700 });
        assert_eq!(
            match validate_register(&future, &enabled) {
                Err(UsernamesError::InvalidBody(errors)) => errors[0].detail.clone(),
                other => panic!("{other:?}", other = other.err()),
            },
            "signedAt is in the future (tolerance 600s)."
        );

        let mut stale = valid_body();
        stale["dotns"] =
            json!({ "signature": format!("0x{}", "ab".repeat(64)), "signedAt": now - 700 });
        assert_eq!(
            match validate_register(&stale, &enabled) {
                Err(UsernamesError::InvalidBody(errors)) => errors[0].detail.clone(),
                other => panic!("{other:?}", other = other.err()),
            },
            "signedAt is older than the intake freshness bound (600s). \
             Re-sign with a fresh timestamp and resubmit."
        );

        let mut valid = valid_body();
        valid["dotns"] = json!({
            "signature": format!("0x{}", "ab".repeat(64)),
            "signedAt": now,
            "reservedUsername": "reservedname"
        });
        let parsed = validate_register(&valid, &enabled).expect("valid dotns");
        let dotns = parsed.dotns.expect("dotns parsed");
        assert_eq!(dotns.reserved_username.as_deref(), Some("reservedname"));
    }

    #[test]
    fn preferred_digit_selection_maps_to_the_frozen_409s() {
        let taken: BTreeSet<u8> = [7u8].into_iter().collect();
        assert_eq!(select_digit(&taken, Some("08"), "base").unwrap(), 8);
        assert!(matches!(
            select_digit(&taken, Some("07"), "base"),
            Err(UsernamesError::PreferredDigitsTaken { .. })
        ));
        let all: BTreeSet<u8> = (1..=99u8).collect();
        assert!(matches!(
            select_digit(&all, None, "base"),
            Err(UsernamesError::NoDigitsAvailable { .. })
        ));
    }

    #[test]
    fn device_token_header_base64_rule_matches_zod() {
        assert!(is_zod_base64(""));
        assert!(is_zod_base64("AgAAABEuCTMX76f2R1TNNVkWUcwEUNk0"));
        assert!(is_zod_base64("YWJj"));
        assert!(is_zod_base64("YQ=="));
        assert!(!is_zod_base64("not base64!!!"));
        assert!(!is_zod_base64("abc"));
        assert!(!is_zod_base64("a==="));
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

        assert!(verify_candidate_signature(
            &candidate,
            &signature,
            &ring_vrf_key
        ));
        assert!(!verify_candidate_signature(
            &candidate, &signature, &[9u8; 32]
        ));
        let other = Keypair::from_uri(&SecretUri::from_str("//Bob").unwrap())
            .unwrap()
            .public_key()
            .0;
        assert!(!verify_candidate_signature(
            &other,
            &signature,
            &ring_vrf_key
        ));
        assert!(!verify_candidate_signature(
            &candidate,
            &[0u8; 10],
            &ring_vrf_key
        ));
    }
}
