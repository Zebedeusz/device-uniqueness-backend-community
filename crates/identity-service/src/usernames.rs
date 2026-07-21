//! `/api/v1/attester` and the identity-owned username API surface
//! (availability reads and registration intake).
//!
//! `available` is chain-backed (`Resources::UsernameOwnerOf`) and JWT-gated, as
//! the shipping clients call it. Public prefix search is owned by the separate
//! `username-indexer` service.

pub(crate) mod available;
pub(crate) mod register;

use std::collections::BTreeSet;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use utoipa::ToSchema;

use crate::http::state::AppState;

/// Minimum base-username length.
const MIN_BASE_LEN: usize = 6;

/// Base-username rules, shared by the read and write paths so they can't
/// disagree: length >= 6, lowercase ASCII letters.
fn is_valid_base(base: &str) -> bool {
    base.len() >= MIN_BASE_LEN && base.bytes().all(|b| b.is_ascii_lowercase())
}

/// The free discriminators for a base: `1..=99` minus the taken set (`00` is
/// never offered). Shared so availability and registration agree on what's free.
fn available_digits(taken: &BTreeSet<u8>) -> Vec<u8> {
    (1..=99u8).filter(|d| !taken.contains(d)).collect()
}

/// Build the `/usernames` router (rate-limited group).
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", post(register::register))
        .route("/available", post(available::check))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::http::middleware::rate_limit,
        ))
}

/// `{ attester }` — the on-chain attester authority public key.
#[derive(Serialize, ToSchema)]
pub struct AttesterResponse {
    /// `0x`-hex sr25519 public key the backend writes registrations as.
    #[schema(example = "0xe4cd20d6d1e0e119d63a943afd2d7496fbbb0ac8e7cd99c3c5f16b63a68e7432")]
    attester: String,
}

/// `GET /api/v1/attester` — the attester authority public key (`0x`+hex, public).
#[utoipa::path(
    get,
    path = "/api/v1/attester",
    tag = "Discovery",
    responses(
        (status = 200, description = "The on-chain attester account the backend writes as.", body = AttesterResponse,
         example = json!({ "attester": "0xe4cd20d6d1e0e119d63a943afd2d7496fbbb0ac8e7cd99c3c5f16b63a68e7432" }))
    )
)]
pub async fn attester(State(state): State<AppState>) -> Json<AttesterResponse> {
    Json(AttesterResponse {
        attester: state.config.attester_public_key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attester_response_keeps_the_mobile_wire_shape() {
        let response = AttesterResponse {
            attester: "0x86aac84d".to_string(),
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "attester": "0x86aac84d" })
        );
    }
}
