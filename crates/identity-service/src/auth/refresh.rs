//! `POST /api/v1/auth/token/refresh` and the rotating refresh-token store.
//!
//! Refresh tokens are opaque and single-use: presenting one atomically marks it
//! spent and mints a replacement, so a stolen-and-reused token fails. The stored
//! row carries the claims needed to re-mint the access token without a new proof.

use axum::extract::State;
use axum::Json;
use base64::Engine as _;
use rand::RngCore as _;
use serde::Deserialize;
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

use super::token::TokenResponse;
use super::B64_STD;
use crate::http::error::{AppError, AppResult};
use crate::http::state::AppState;

#[derive(Deserialize)]
pub struct RefreshRequest {
    #[serde(rename = "refreshToken")]
    refresh_token: String,
}

/// Claims carried on a refresh row, used to re-mint the access token.
struct Session {
    account_id: String,
    app_from_official_store: bool,
    platform: Option<String>,
}

/// Rotate a refresh token, returning a fresh access + refresh pair.
pub async fn rotate(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> AppResult<Json<TokenResponse>> {
    match rotate_token(&state.pool, &req.refresh_token, state.config.refresh_ttl).await? {
        Some((session, refresh_token)) => {
            let token = state.jwt.issue(
                &session.account_id,
                session.app_from_official_store,
                session.platform.as_deref(),
                state.config.access_ttl,
            );
            Ok(Json(TokenResponse::new(token, refresh_token)))
        }
        None => Err(AppError::Unauthorized),
    }
}

/// Mint and persist a new refresh token for an account.
pub async fn mint(
    pool: &PgPool,
    account_id: &str,
    app_from_official_store: bool,
    platform: Option<&str>,
    ttl: std::time::Duration,
) -> Result<String, sqlx::Error> {
    let token = random_token();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::seconds(ttl.as_secs() as i64);
    sqlx::query(
        "INSERT INTO refresh_tokens (token, account_id, app_from_official_store, platform, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&token)
    .bind(account_id)
    .bind(app_from_official_store)
    .bind(platform)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(token)
}

/// Atomically spend `presented` and issue its replacement.
async fn rotate_token(
    pool: &PgPool,
    presented: &str,
    ttl: std::time::Duration,
) -> Result<Option<(Session, String)>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let Some(row) = sqlx::query(
        "SELECT account_id, app_from_official_store, platform, used_at, expires_at \
         FROM refresh_tokens WHERE token = $1 FOR UPDATE",
    )
    .bind(presented)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };

    let used_at: Option<OffsetDateTime> = row.try_get("used_at")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if used_at.is_some() || expires_at <= OffsetDateTime::now_utc() {
        return Ok(None);
    }

    let session = Session {
        account_id: row.try_get("account_id")?,
        app_from_official_store: row.try_get("app_from_official_store")?,
        platform: row.try_get("platform")?,
    };

    let new_token = random_token();
    let new_expires = OffsetDateTime::now_utc() + time::Duration::seconds(ttl.as_secs() as i64);
    sqlx::query("UPDATE refresh_tokens SET used_at = now(), replaced_by = $2 WHERE token = $1")
        .bind(presented)
        .bind(&new_token)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO refresh_tokens (token, account_id, app_from_official_store, platform, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&new_token)
    .bind(&session.account_id)
    .bind(session.app_from_official_store)
    .bind(&session.platform)
    .bind(new_expires)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Some((session, new_token)))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    B64_STD.encode(bytes)
}
