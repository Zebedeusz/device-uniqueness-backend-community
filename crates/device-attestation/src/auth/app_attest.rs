//! `POST /api/v1/auth/app-attest/attestations` — iOS Apple App Attest key
//! registration; the assertion side lives in the `/auth/token` handler.
//!
//! With `AUTH_ENABLED=false` this stays the shape-preserving no-op (`202 {}`)
//! — byte-identical M0/testnet behavior. Enabled, the attestation object is
//! verified against the Apple root ([`verify`]) and the credential key
//! persisted ([`store`]); failures mirror the legacy `401 {_tag, error}` pair.

// `pub` (not `pub(crate)`) so lib.rs can re-export it for the live-PG suite;
// the enclosing modules stay private, so the only external path is the
// `#[doc(hidden)]` alias.
pub mod store;
pub(crate) mod verify;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

use super::{challenge, B64_STD};
use crate::http::error::{AppError, AppResult};
use crate::http::state::AppState;

/// iOS Apple App Attest key-registration payload.
#[derive(Deserialize, ToSchema)]
pub struct AppAttestRequest {
    /// Base64 key identifier (SHA-256 of the credential public key).
    #[serde(rename = "keyId")]
    #[schema(
        rename = "keyId",
        example = "s/134MbeEEZDZKCvOTf+jZgNhpoDwdXZ8cKfTym8FUg="
    )]
    key_id: String,
    /// Base64 challenge previously returned from `/auth/challenges`.
    #[serde(rename = "challenge")]
    #[schema(example = "challenge-from-/auth/challenges")]
    challenge: String,
    /// Base64 CBOR attestation object from the platform.
    #[serde(rename = "attestation")]
    #[schema(example = "base64-attestation-object")]
    attestation: String,
}

/// Register an App Attest key: verify the attestation object and persist the
/// credential key (no-op `202 {}` while `AUTH_ENABLED=false`).
#[utoipa::path(
    post,
    path = "/api/v1/auth/app-attest/attestations",
    tag = "Authentication",
    request_body = AppAttestRequest,
    responses(
        (status = 202, description = "Accepted. With attestation enabled, the key is verified \
            against Apple and stored; while disabled, the payload is not verified.",
         body = serde_json::Value,
         example = json!({})),
        (status = 401, description = "Attestation rejected (hard mode), or the challenge was \
            unknown, expired, or already used. Soft mode logs failed verdicts and returns 202.",
         body = serde_json::Value,
         example = json!({ "_tag": "VERIFY_ATTESTATION_FAILED", "error": "attestation nonce mismatch" }))
    )
)]
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<AppAttestRequest>,
) -> AppResult<Response> {
    let accepted = (StatusCode::ACCEPTED, Json(json!({})));
    if !state.config.auth_enabled {
        return Ok(accepted.into_response());
    }

    let key_id = decode_b64_field(&req.key_id, "keyId")?;
    let challenge_bytes = decode_b64_field(&req.challenge, "challenge")?;
    let attestation = decode_b64_field(&req.attestation, "attestation")?;

    if !challenge::consume(&state.pool, &challenge_bytes).await? {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "_tag": "CHALLENGE_NOT_FOUND", "error": "Challenge Not Found" })),
        )
            .into_response());
    }

    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    match verify::verify_attestation(
        &attestation,
        &challenge_bytes,
        &key_id,
        &state.config.apple_app_attest_app_ids,
        &verify::apple_root_ca_der(),
        now,
    ) {
        Ok(attested) => {
            store::upsert(
                &state.pool,
                &key_id,
                &attested.public_key,
                &attested.receipt,
                None,
            )
            .await?;
            tracing::info!(key_id = %hex::encode(&key_id), "app attest key registered");
            Ok(accepted.into_response())
        }
        Err(err) if state.config.enforce_auth => {
            tracing::warn!(key_id = %hex::encode(&key_id), error = %err, "app attest attestation rejected (hard mode)");
            Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "_tag": "VERIFY_ATTESTATION_FAILED", "error": err.to_string() })),
            )
                .into_response())
        }
        Err(err) => {
            // Soft mode logs the verdict and accepts without storing the
            // unverified key (assertions against it will fail their own
            // posture-gated verdict at /auth/token).
            tracing::warn!(key_id = %hex::encode(&key_id), error = %err, "app attest attestation failed (soft mode, request allowed)");
            Ok(accepted.into_response())
        }
    }
}

/// Decode a required base64 body field.
fn decode_b64_field(value: &str, name: &str) -> AppResult<Vec<u8>> {
    B64_STD
        .decode(value.trim())
        .map_err(|_| AppError::bad_request(format!("body field {name} is not valid base64")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_app_attest_contract_requires_all_camel_case_fields() {
        assert!(serde_json::from_value::<AppAttestRequest>(json!({
            "keyId": "app-attest-key",
            "challenge": "challenge"
        }))
        .is_err());

        let request = serde_json::from_value::<AppAttestRequest>(json!({
            "keyId": "a2V5",
            "challenge": "Y2hhbGxlbmdl",
            "attestation": "YXR0ZXN0YXRpb24="
        }))
        .unwrap();
        assert_eq!(decode_b64_field(&request.key_id, "keyId").unwrap(), b"key");
        assert_eq!(
            decode_b64_field(&request.challenge, "challenge").unwrap(),
            b"challenge"
        );
        assert_eq!(
            decode_b64_field(&request.attestation, "attestation").unwrap(),
            b"attestation"
        );
    }
}
