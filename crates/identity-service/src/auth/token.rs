//! `POST /api/v1/auth/token` — verify the account proof, issue a JWT.
//!
//! Attestation (`Auth-Payload`, `attestationChain`, App Attest / Play Integrity)
//! is a no-op while `AUTH_ENABLED` is false — but the challenge and sr25519
//! client proof are always verified, so `accountId` is bound to a key the caller
//! controls. The proof hashes the **raw** request body, so we read it as bytes
//! rather than parsing JSON (re-serialization would change the hash).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Serialize;

use super::{challenge, decode_header, decode_header_fixed, detect_platform, proof, refresh};
use crate::http::error::{AppError, AppResult};
use crate::http::state::AppState;

/// `{ token, refreshToken }` — the shape both auth token routes return.
#[derive(Serialize)]
pub struct TokenResponse {
    token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
}

impl TokenResponse {
    pub(crate) fn new(token: String, refresh_token: String) -> Self {
        Self {
            token,
            refresh_token,
        }
    }
}

/// Verify challenge + client proof and issue an access + refresh token pair.
pub async fn issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<TokenResponse>> {
    let client_id: [u8; 32] = decode_header_fixed(&headers, "auth-clientid")?;
    let client_proof: [u8; 64] = decode_header_fixed(&headers, "auth-clientproof")?;
    let challenge = decode_header(&headers, "auth-challenge")?;

    // Single-use + TTL: consume before verifying so a valid challenge can't be replayed.
    if !challenge::consume(&state.pool, &challenge).await? {
        return Err(AppError::Unauthorized);
    }
    if !proof::verify(&challenge, &client_id, &body, &client_proof) {
        return Err(AppError::Unauthorized);
    }

    if state.config.auth_enabled {
        // Slice 6 lands real App Attest / Play Integrity / TEE verification here.
        tracing::debug!("attestation enabled: platform verification not yet implemented");
    } else {
        tracing::debug!(
            attestation = state.config.attestation_mode(),
            "attestation no-op"
        );
    }

    let account_id = format!("0x{}", hex::encode(client_id));
    let platform = detect_platform(&headers);
    // Attestation-derived in Slice 6; defaults true (iOS App Store contract) for now.
    let app_from_official_store = true;

    let token = state.jwt.issue(
        &account_id,
        app_from_official_store,
        platform,
        state.config.access_ttl,
    );
    let refresh_token = refresh::mint(
        &state.pool,
        &account_id,
        app_from_official_store,
        platform,
        state.config.refresh_ttl,
    )
    .await?;

    Ok(Json(TokenResponse::new(token, refresh_token)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_response_keeps_the_mobile_wire_shape() {
        let response = TokenResponse::new("access".to_string(), "refresh".to_string());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "token": "access", "refreshToken": "refresh" })
        );
    }
}
