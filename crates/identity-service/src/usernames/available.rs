//! `POST /api/v1/usernames/available` — chain-backed availability + digits.
//!
//! The response is the v1 `{_tag: "v1", value: …}` record with
//! `availableDigits`; body validation renders problem-details 400s and
//! malformed JSON a plain 400. The captured legacy quirk where a missing
//! JSON content type produced an opaque 500 (the legacy handler crashed on
//! an unvalidated body) was retired 2026-07-23: the body now parses as JSON
//! regardless of content type.
//!
//! Divergences from the capture, recorded in the plan doc: legacy served this
//! route unauthenticated with no rate limit; here it keeps the JWT gate (the
//! frozen 401 trio) and the subject rate limit as deliberate hardening. The
//! allocation source is People Chain `UsernameOwnerOf` plus the durable
//! reservation outbox, not the legacy Postgres table. The legacy-default
//! flat v0 record was removed 2026-07-23 (no client uses it — both shipping
//! apps send `?version=v1`): the response is always v1, `?version=v0` is now
//! rejected like any unknown version, and a missing `?version` defaults to
//! v1.

use std::collections::{BTreeMap, HashMap};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_common::{zod, AuthSubject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::http::state::AppState;

use super::error::{FieldError, UsernamesError, UsernamesResult};
use super::{available_digits, is_valid_base, taken_discriminators};

/// Max base names per request (the legacy `z.array(...).max(100)`).
const MAX_USERNAMES: usize = 100;

/// `{ usernames: [base, ...] }` — bases to validate and check on chain
/// (documentation mirror; the handler validates raw JSON).
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct AvailableRequest {
    /// Candidate base usernames to validate and check (max 100).
    #[schema(example = json!(["tallesx", "abc"]))]
    usernames: Vec<String>,
}

/// `{ _tag: "v1", value: { base: { status, availableDigits? } } }`.
#[derive(Serialize, ToSchema)]
pub struct AvailableV1Response {
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

/// Availability for each requested base.
#[utoipa::path(
    post,
    path = "/api/v1/usernames/available",
    tag = "Usernames",
    security(("bearer_jwt" = [])),
    params(("version" = Option<String>, Query, description = "Response version; `v1` is the only (and default) option. The legacy flat `v0` record was removed 2026-07-23.")),
    request_body = AvailableRequest,
    responses(
        (status = 200, description = "Per-base availability read from People Chain UsernameOwnerOf plus pending outbox reservations, tagged `{_tag: \"v1\", value}` with availableDigits.",
         body = AvailableV1Response,
         example = json!({ "_tag": "v1", "value": {
             "tallesx": { "status": "AVAILABLE", "availableDigits": [1, 2, 3] },
             "abc": { "status": "INVALID" }
         } })),
        (status = 400, description = "Invalid `version` (query dialect), invalid `usernames` (body dialect), or malformed JSON.",
         body = serde_json::Value,
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-body-property-value",
             "title": "Invalid Body Property Value",
             "detail": "The request body contains an invalid body property value.",
             "status": 400,
             "errors": [{ "detail": "Too big: expected array to have <=100 items", "pointer": "#/usernames" }]
         })),
        (status = 401, description = "Missing or invalid bearer token (the frozen problem-details trio; a deliberate hardening divergence — legacy served this route unauthenticated).",
         body = serde_json::Value),
        (status = 429, description = "Subject rate limit exceeded (problem details with `Retry-After`).",
         body = serde_json::Value),
        (status = 500, description = "People Chain failure or reservation-outbox failure.",
         body = serde_json::Value,
         example = json!({ "error": "Oops! Something went wrong." }))
    )
)]
pub async fn check(
    State(state): State<AppState>,
    auth: AuthSubject,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> UsernamesResult<Response> {
    super::check_rate_limit(&state, &auth.subject)?;

    ensure_v1_version(&params)?;

    let value = super::parse_json_body(&body)?;
    let usernames = validate_usernames(&value)?;

    let mut value_map = BTreeMap::new();
    for base in usernames {
        let availability = availability_for(&state, &base).await?;
        value_map.insert(base, availability);
    }

    let response = AvailableV1Response {
        tag: "v1",
        value: value_map,
    };
    Ok(Json(response).into_response())
}

/// `?version=v1` (or absent — v1 is the default). `v0` was removed
/// 2026-07-23 and rejects like any unknown version.
fn ensure_v1_version(params: &HashMap<String, String>) -> UsernamesResult<()> {
    match params.get("version").map(String::as_str) {
        None | Some("v1") => Ok(()),
        Some(_) => Err(UsernamesError::InvalidQuery(vec![FieldError {
            detail: "Invalid option: expected one of \"v1\"".to_string(),
            pointer: "#/version".to_string(),
        }])),
    }
}

/// Validate `{usernames: string[≤100]}` with the captured zod messages.
fn validate_usernames(value: &Value) -> UsernamesResult<Vec<String>> {
    let field = value.as_object().and_then(|o| o.get("usernames"));
    let Some(Value::Array(items)) = field else {
        return Err(UsernamesError::InvalidBody(vec![FieldError {
            detail: zod::invalid_input("array", zod::received_name(field)),
            pointer: "#/usernames".to_string(),
        }]));
    };

    let mut errors = Vec::new();
    let mut usernames = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        match item {
            Value::String(s) => usernames.push(s.clone()),
            other => errors.push(FieldError {
                detail: zod::invalid_input("string", zod::type_name(other)),
                pointer: format!("#/usernames/{index}"),
            }),
        }
    }
    if items.len() > MAX_USERNAMES {
        errors.push(FieldError {
            detail: zod::too_big_array(MAX_USERNAMES),
            pointer: "#/usernames".to_string(),
        });
    }
    if !errors.is_empty() {
        return Err(UsernamesError::InvalidBody(errors));
    }
    Ok(usernames)
}

async fn availability_for(state: &AppState, base: &str) -> UsernamesResult<NameAvailability> {
    if !is_valid_base(base) {
        return Ok(NameAvailability {
            status: "INVALID",
            available_digits: None,
        });
    }

    let taken = taken_discriminators(state, base).await?;
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
    fn v1_response_matches_the_captured_shape() {
        let response = AvailableV1Response {
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
        let response = AvailableV1Response {
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

    #[test]
    fn version_defaults_to_v1_and_rejects_v0_and_unknown_values() {
        assert!(ensure_v1_version(&HashMap::new()).is_ok());
        assert!(ensure_v1_version(&HashMap::from([("version".into(), "v1".into())])).is_ok());
        for removed in ["v0", "v2"] {
            match ensure_v1_version(&HashMap::from([("version".into(), removed.into())])) {
                Err(UsernamesError::InvalidQuery(errors)) => {
                    assert_eq!(errors[0].pointer, "#/version");
                    assert_eq!(errors[0].detail, "Invalid option: expected one of \"v1\"");
                }
                other => panic!("expected InvalidQuery, got {other:?}", other = other.err()),
            }
        }
    }

    #[test]
    fn usernames_validation_uses_the_captured_messages() {
        let cases = [
            (
                json!({}),
                "Invalid input: expected array, received undefined",
            ),
            (
                json!({ "usernames": null }),
                "Invalid input: expected array, received null",
            ),
            (
                json!({ "usernames": "openok" }),
                "Invalid input: expected array, received string",
            ),
        ];
        for (body, expected) in cases {
            match validate_usernames(&body) {
                Err(UsernamesError::InvalidBody(errors)) => {
                    assert_eq!(errors.len(), 1);
                    assert_eq!(errors[0].pointer, "#/usernames");
                    assert_eq!(errors[0].detail, expected);
                }
                other => panic!("expected InvalidBody, got {other:?}", other = other.err()),
            }
        }

        match validate_usernames(&json!({ "usernames": [5] })) {
            Err(UsernamesError::InvalidBody(errors)) => {
                assert_eq!(errors[0].pointer, "#/usernames/0");
                assert_eq!(
                    errors[0].detail,
                    "Invalid input: expected string, received number"
                );
            }
            other => panic!("expected InvalidBody, got {other:?}", other = other.err()),
        }

        let hundred_one: Vec<Value> = (0..101).map(|i| json!(format!("user{i}xx"))).collect();
        match validate_usernames(&json!({ "usernames": hundred_one })) {
            Err(UsernamesError::InvalidBody(errors)) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(
                    errors[0].detail,
                    "Too big: expected array to have <=100 items"
                );
            }
            other => panic!("expected InvalidBody, got {other:?}", other = other.err()),
        }

        assert!(validate_usernames(&json!({ "usernames": [] }))
            .expect("empty array is valid")
            .is_empty());
    }
}
