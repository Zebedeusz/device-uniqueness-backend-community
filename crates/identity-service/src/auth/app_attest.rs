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

#[derive(Deserialize)]
pub struct AppAttestRequest {
    #[serde(rename = "keyId")]
    _key_id: String,
    #[serde(rename = "challenge")]
    _challenge: String,
    #[serde(rename = "attestation")]
    _attestation: String,
}

/// Accept the attestation payload and acknowledge with `202` (no-op for now).
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
