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
