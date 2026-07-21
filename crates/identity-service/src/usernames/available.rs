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
use utoipa::ToSchema;

use crate::http::error::{AppError, AppResult};
use crate::http::extract::AuthAccount;
use crate::http::state::AppState;

use super::{available_digits, is_valid_base};

/// Max base names per request (matches the reference backend).
const MAX_USERNAMES: usize = 100;

/// `{ usernames: [base, ...] }` — bases to validate and check on chain.
#[derive(Deserialize, ToSchema)]
pub struct AvailableRequest {
    /// Candidate base usernames to validate and check (max 100).
    #[schema(example = json!(["tallesx", "abc"]))]
    usernames: Vec<String>,
}

/// `{ _tag: "v1", value: { base: {status, availableDigits?} } }`.
#[derive(Serialize, ToSchema)]
pub struct AvailableResponse {
    /// Response schema tag; always `v1`.
    #[serde(rename = "_tag")]
    #[schema(rename = "_tag", example = "v1")]
    tag: &'static str,
    /// Per-base availability, keyed by the requested base username.
    value: BTreeMap<String, NameAvailability>,
}

/// Availability of a single base username.
#[derive(Serialize, ToSchema)]
pub(crate) struct NameAvailability {
    /// `AVAILABLE`, `EXHAUSTED` (all 100 taken), or `INVALID` (fails base rules).
    #[schema(example = "AVAILABLE")]
    status: &'static str,
    /// Free discriminators (`1..=99`); present only when `AVAILABLE`.
    #[serde(rename = "availableDigits", skip_serializing_if = "Option::is_none")]
    #[schema(rename = "availableDigits", example = json!([1, 2, 3]))]
    available_digits: Option<Vec<u8>>,
}

/// Availability for each requested base (JWT-gated).
#[utoipa::path(
    post,
    path = "/api/v1/usernames/available",
    tag = "Usernames",
    security(("bearer_jwt" = [])),
    params(("version" = Option<String>, Query, description = "Response schema tag selector, e.g. v1.")),
    request_body = AvailableRequest,
    responses(
        (status = 200, description = "Per-base availability read from People Chain UsernameOwnerOf.",
         body = AvailableResponse,
         example = json!({ "_tag": "v1", "value": {
             "tallesx": { "status": "AVAILABLE", "availableDigits": [1, 2, 3] },
             "abc": { "status": "INVALID" }
         } })),
        (status = 400, description = "More than 100 usernames per request.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "WRONG_DATA", "message": "at most 100 usernames per request" })),
        (status = 401, description = "Missing or invalid bearer token.",
         body = crate::http::error::ErrorResponse,
         example = json!({ "error": "UNAUTHORIZED", "message": "unauthorized" }))
    )
)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn availability_contract_matches_ios_and_android() {
        let request = serde_json::from_value::<AvailableRequest>(json!({
            "usernames": ["aliceuser"]
        }))
        .unwrap();
        assert_eq!(request.usernames, ["aliceuser"]);

        let response = AvailableResponse {
            tag: "v1",
            value: BTreeMap::from([(
                "aliceuser".to_string(),
                NameAvailability {
                    status: "AVAILABLE",
                    available_digits: Some(vec![1, 42, 99]),
                },
            )]),
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "_tag": "v1",
                "value": {
                    "aliceuser": {
                        "status": "AVAILABLE",
                        "availableDigits": [1, 42, 99]
                    }
                }
            })
        );
    }

    #[test]
    fn unavailable_names_omit_available_digits() {
        let response = AvailableResponse {
            tag: "v1",
            value: BTreeMap::from([(
                "short".to_string(),
                NameAvailability {
                    status: "INVALID",
                    available_digits: None,
                },
            )]),
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "_tag": "v1", "value": { "short": { "status": "INVALID" } } })
        );
    }
}
