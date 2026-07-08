//! Request extractors.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use super::error::AppError;
use super::state::AppState;

/// An authenticated account, extracted from a verified `Authorization: Bearer` JWT.
///
/// Rejects with `401` when the header is missing or the token fails verification.
pub struct AuthAccount {
    /// `0x`-hex sr25519 public key of the caller (the JWT subject).
    pub account_id: String,
}

impl FromRequestParts<AppState> for AuthAccount {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        let claims = state
            .jwt
            .verify(token)
            .map_err(|_| AppError::Unauthorized)?;
        Ok(AuthAccount {
            account_id: claims.account_id,
        })
    }
}
