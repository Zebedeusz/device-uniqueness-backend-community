//! `POST /api/v1/auth/app-attest/attestations` — iOS Apple App Attest key
//! registration.
//!
//! Shape-preserving no-op returning `202 {}` while `AUTH_ENABLED` is false. The
//! real App Attest verification + key persistence lands in Slice 6; the route
//! exists now because the shipping iOS client calls it.

use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

/// iOS Apple App Attest key-registration payload. Shape-preserving; not verified
/// while `AUTH_ENABLED` is false.
#[derive(Deserialize, ToSchema)]
pub struct AppAttestRequest {
    /// Client key identifier from App Attest enrollment.
    #[serde(rename = "keyId")]
    #[schema(rename = "keyId", example = "base64-or-client-key-id")]
    _key_id: String,
    /// Challenge previously returned from `/auth/challenges`.
    #[serde(rename = "challenge")]
    #[schema(example = "challenge-from-/auth/challenges")]
    _challenge: String,
    /// Base64 attestation object from the platform.
    #[serde(rename = "attestation")]
    #[schema(example = "base64-attestation-object")]
    _attestation: String,
}

/// Accept the attestation payload and acknowledge with `202` (no-op for now).
#[utoipa::path(
    post,
    path = "/api/v1/auth/app-attest/attestations",
    tag = "Authentication",
    request_body = AppAttestRequest,
    responses(
        (status = 202, description = "Accepted. No-op in M0 — payload is not verified against Apple/Google.",
         body = serde_json::Value,
         example = json!({}))
    )
)]
pub async fn register(Json(_req): Json<AppAttestRequest>) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::ACCEPTED, Json(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ios_app_attest_contract_is_accepted_while_auth_is_off() {
        let request = serde_json::from_value::<AppAttestRequest>(json!({
            "keyId": "app-attest-key",
            "challenge": "challenge",
            "attestation": "attestation-object"
        }))
        .unwrap();

        let (status, Json(body)) = register(Json(request)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, json!({}));
    }

    #[test]
    fn ios_app_attest_contract_requires_all_camel_case_fields() {
        assert!(serde_json::from_value::<AppAttestRequest>(json!({
            "keyId": "app-attest-key",
            "challenge": "challenge"
        }))
        .is_err());
    }
}
