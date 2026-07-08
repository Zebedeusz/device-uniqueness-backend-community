//! `/api/v1/auth/*` — the attestation → JWT handshake.
//!
//! Byte-compatible with the shipping iOS/Android clients (paths, headers, and
//! JSON shapes verified against the reference backend). While `AUTH_ENABLED` is
//! false, platform attestation is a no-op, but the sr25519 account proof is
//! always verified — that is what binds the JWT `accountId` to a key the caller
//! controls.

mod app_attest;
mod challenge;
mod proof;
mod refresh;
mod token;

use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use base64::Engine as _;

use crate::http::error::{AppError, AppResult};
use crate::http::state::AppState;

const B64_STD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Build the auth router with the rate-limit layer applied to the whole group.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/challenges", post(challenge::issue))
        .route("/app-attest/attestations", post(app_attest::register))
        .route("/token", post(token::issue))
        // The single clean refresh path; the prod double-mount is not reproduced.
        .route("/token/refresh", post(refresh::rotate))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::http::middleware::rate_limit,
        ))
}

/// Decode a required base64 header into raw bytes.
pub(crate) fn decode_header(headers: &HeaderMap, name: &str) -> AppResult<Vec<u8>> {
    let raw = headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::bad_request(format!("missing or invalid header {name}")))?;
    B64_STD
        .decode(raw.trim())
        .map_err(|_| AppError::bad_request(format!("header {name} is not valid base64")))
}

/// Decode a required base64 header into a fixed-size byte array.
pub(crate) fn decode_header_fixed<const N: usize>(
    headers: &HeaderMap,
    name: &str,
) -> AppResult<[u8; N]> {
    decode_header(headers, name)?
        .try_into()
        .map_err(|_| AppError::bad_request(format!("header {name} must decode to {N} bytes")))
}

/// Derive the platform claim from the optional package headers.
pub(crate) fn detect_platform(headers: &HeaderMap) -> Option<&'static str> {
    if headers.contains_key("auth-ios-package") {
        Some("ios")
    } else if headers.contains_key("auth-android-package") {
        Some("android")
    } else {
        None
    }
}
