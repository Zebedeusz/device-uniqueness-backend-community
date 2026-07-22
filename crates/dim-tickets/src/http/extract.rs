//! Request extractors: verify-only JWT auth.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use super::error::AppError;
use super::state::AppState;

/// An authenticated subject, extracted from a verified `Authorization: Bearer`
/// JWT (identity-service tokens, verified via the shared `jwt-verify` crate).
///
/// Rejections reproduce the legacy middleware's three 401 problem-details
/// variants: missing header, non-Bearer header, failed verification.
pub struct AuthSubject {
    /// `0x`-hex sr25519 public key of the caller (the JWT subject).
    pub subject: String,
}

/// Split an `Authorization` header value into its Bearer token, legacy-style
/// (any whitespace run separates exactly two parts).
fn bearer_token(header: &str) -> Option<&str> {
    let mut parts = header.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("Bearer"), Some(token), None) if !token.is_empty() => Some(token),
        _ => None,
    }
}

impl FromRequestParts<AppState> for AuthSubject {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AppError::MissingAuthHeader)?;
        let token = bearer_token(header).ok_or(AppError::InvalidAuthHeader)?;
        let claims = state
            .verifier
            .verify(token)
            .map_err(|_| AppError::InvalidToken)?;
        Ok(AuthSubject {
            subject: claims.account_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::bearer_token;

    #[test]
    fn parses_bearer_scheme_only() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("Bearer   abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), None); // scheme is case-sensitive, as legacy
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None);
        assert_eq!(bearer_token("Bearer a b"), None);
    }
}
