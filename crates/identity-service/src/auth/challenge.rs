//! `POST /api/v1/auth/challenges` and the challenge store.
//!
//! Upgrades the reference backend's stateless HMAC challenge to a DB-backed,
//! single-use, TTL'd nonce. The wire value stays an opaque base64 blob the
//! client echoes back in `Auth-Challenge`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::Engine as _;
use rand::RngCore as _;
use serde::Serialize;
use sqlx::PgPool;

use crate::http::error::AppResult;
use crate::http::state::AppState;

use super::B64_STD;

#[derive(Serialize)]
pub struct ChallengeResponse {
    challenge: String,
}

/// Issue a fresh single-use challenge (`201`).
pub async fn issue(
    State(state): State<AppState>,
) -> AppResult<(StatusCode, Json<ChallengeResponse>)> {
    let challenge = create(&state.pool, state.config.challenge_ttl).await?;
    Ok((StatusCode::CREATED, Json(ChallengeResponse { challenge })))
}

/// Generate + persist a challenge, returning its base64 wire form.
async fn create(pool: &PgPool, ttl: std::time::Duration) -> Result<String, sqlx::Error> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let expires_at =
        time::OffsetDateTime::now_utc() + time::Duration::seconds(ttl.as_secs() as i64);

    sqlx::query("INSERT INTO auth_challenges (challenge, expires_at) VALUES ($1, $2)")
        .bind(&bytes[..])
        .bind(expires_at)
        .execute(pool)
        .await?;

    Ok(B64_STD.encode(bytes))
}

/// Atomically consume a challenge; `true` if it was valid, unexpired, unused.
pub async fn consume(pool: &PgPool, challenge: &[u8]) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE auth_challenges SET consumed_at = now() \
         WHERE challenge = $1 AND consumed_at IS NULL AND expires_at > now()",
    )
    .bind(challenge)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn challenge_response_keeps_the_mobile_wire_shape() {
        let challenge = B64_STD.encode([7u8; 32]);
        let response = ChallengeResponse {
            challenge: challenge.clone(),
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "challenge": challenge })
        );
        assert_eq!(B64_STD.decode(challenge).unwrap().len(), 32);
    }
}
