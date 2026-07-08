//! `POST /api/v1/usernames/available` — chain-backed availability + digits.
//!
//! Byte-compatible with the shipping clients' `?version=v1` call: body
//! `{usernames: [base,...]}`, response `{_tag: "v1", value: {base: {status,
//! availableDigits?}}}`. A base is `AVAILABLE` when at least one `{base}.{NN}`
//! discriminator is unowned on chain, `EXHAUSTED` when all 100 are taken, and
//! `INVALID` when it fails the base rules (lowercase letters, length >= 6).

use std::collections::BTreeMap;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::http::error::{AppError, AppResult};
use crate::http::extract::AuthAccount;
use crate::http::state::AppState;

use super::{available_digits, is_valid_base};

/// Max base names per request (matches the reference backend).
const MAX_USERNAMES: usize = 100;

#[derive(Deserialize)]
pub struct AvailableRequest {
    usernames: Vec<String>,
}

#[derive(Serialize)]
pub struct AvailableResponse {
    #[serde(rename = "_tag")]
    tag: &'static str,
    value: BTreeMap<String, NameAvailability>,
}

#[derive(Serialize)]
struct NameAvailability {
    status: &'static str,
    #[serde(rename = "availableDigits", skip_serializing_if = "Option::is_none")]
    available_digits: Option<Vec<u8>>,
}

/// Availability for each requested base (JWT-gated).
pub async fn check(
    State(state): State<AppState>,
    account: AuthAccount,
    Json(req): Json<AvailableRequest>,
) -> AppResult<Json<AvailableResponse>> {
    if req.usernames.len() > MAX_USERNAMES {
        return Err(AppError::bad_request(format!(
            "at most {MAX_USERNAMES} usernames per request"
        )));
    }
    tracing::debug!(account = %account.account_id, count = req.usernames.len(), "availability check");

    let mut value = BTreeMap::new();
    for base in req.usernames {
        let availability = availability_for(&state, &base).await?;
        value.insert(base, availability);
    }
    Ok(Json(AvailableResponse { tag: "v1", value }))
}

async fn availability_for(state: &AppState, base: &str) -> AppResult<NameAvailability> {
    if !is_valid_base(base) {
        return Ok(NameAvailability {
            status: "INVALID",
            available_digits: None,
        });
    }

    let taken = state.chain.taken_discriminators(base).await?;
    if taken.len() >= 100 {
        return Ok(NameAvailability {
            status: "EXHAUSTED",
            available_digits: None,
        });
    }

    // Pool is 01..=99 (00 is never offered); available = pool minus taken.
    let digits = available_digits(&taken);
    Ok(NameAvailability {
        status: "AVAILABLE",
        available_digits: Some(digits),
    })
}
