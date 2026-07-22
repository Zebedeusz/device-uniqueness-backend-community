//! HTTP surface for `dim-tickets-api`: the two frozen legacy routes plus the
//! shared axum primitives (state, errors — the rest lives in `http-common`).
//!
//! Route behavior — including its quirks — is the legacy contract, golden-
//! captured from the real TypeScript route (`tests/fixtures/legacy-wire.json`):
//! - `POST /api/v1/dim-ticket`: auth (401, the shared middleware ran first in
//!   legacy too) → body-parse (unparseable JSON → the Hono plain-text 400;
//!   **missing JSON content-type → 500**, because legacy silently skipped
//!   validation and the handler then crashed) → body-validate (400 problem
//!   details carrying zod v4 messages; NO SS58 check at this layer) →
//!   per-subject rate limit (429) → guard `who != inviter` (500, the legacy
//!   `die`) → SS58-parse `who` (**500**, not 400 — legacy validated SS58 in
//!   the shell where its typed error fell through to the global error hook) →
//!   on-chain quota (≤0 → 422) → insert (conflict → 409) → 200.
//! - `GET /api/v1/dim-ticket/{who}`: auth → rate limit → lookup (missing →
//!   404) → 200. No SS58 validation at all, exactly like legacy.
//! - Unmatched paths and wrong methods: the Hono default `404 Not Found`
//!   plain-text body (Hono routes are method-specific; a wrong method on a
//!   known path was a 404, never a 405).

pub mod error;
pub mod state;

use std::str::FromStr as _;

use anyhow::Context as _;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_common::problem::hono_not_found;
use http_common::{health, layers, zod, AuthSubject};
use time::OffsetDateTime;

use self::error::{AppError, AppResult, FieldError};
use self::state::AppState;
use crate::tickets::{self, Dim};

/// Assemble the full router: health at root, the frozen `/api/v1/dim-ticket`
/// surface below, wrapped in the production middleware stack.
pub fn routes(state: AppState) -> Router {
    let api = Router::new()
        .route("/", post(create_ticket).fallback(hono_not_found))
        .route("/{who}", get(get_ticket).fallback(hono_not_found));

    let router = Router::new()
        .merge(health::router("dim-tickets", readiness))
        .nest("/api/v1/dim-ticket", api)
        .fallback(hono_not_found)
        .with_state(state);
    layers::standard_layers(router)
}

/// Readiness gates on Postgres and the People Chain RPC — the create path
/// cannot answer its quota check without the chain.
async fn readiness(state: AppState) -> health::Readiness {
    if let Err(err) = sqlx::query("SELECT 1").execute(&state.pool).await {
        tracing::warn!(error = ?err, "readiness check failed: database unavailable");
        return Err("db");
    }
    if let Err(err) = state.chain.health().await {
        tracing::warn!(error = ?err, "readiness check failed: People Chain unavailable");
        return Err("chain");
    }
    Ok(&["db", "chain"])
}

/// Enforce the per-subject fixed window; `429` with `Retry-After` on excess.
fn check_rate_limit(state: &AppState, subject: &str) -> Result<(), AppError> {
    if state.limiter.allow(subject) {
        Ok(())
    } else {
        Err(AppError::RateLimited {
            retry_after_secs: state.limiter.window_secs(),
        })
    }
}

/// Look up a DIM ticket's registration status (JWT-gated).
#[utoipa::path(
    get,
    path = "/api/v1/dim-ticket/{who}",
    tag = "DIM Tickets",
    security(("bearer_jwt" = [])),
    params(
        ("who" = String, Path, description = "SS58 address the ticket was requested for \
(not validated — an arbitrary string is looked up verbatim, exactly like legacy)."),
    ),
    responses(
        (status = 200, description = "The ticket's current status. DB `SUBMITTING` reports as \
`SUBMITTED` on the wire; `registered` is the deprecated boolean mirror of \
`status == \"REGISTERED\"`; `onchainData` is `null` until registration.",
         body = crate::openapi::TicketStatusResponse),
        (status = 401, description = "Missing/malformed `Authorization` header or failed token \
verification (RFC 9457 problem details).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/unauthorized",
             "title": "Invalid Token",
             "detail": "Token verification failed. The token may be expired or malformed.",
             "status": 401
         })),
        (status = 404, description = "No ticket row for this address.",
         example = json!({ "error": "Ticket not found" })),
        (status = 429, description = "Per-subject rate limit exceeded (RFC 9457 problem \
details, with `Retry-After`).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/too-many-requests",
             "title": "Too Many Requests",
             "detail": "Rate limit exceeded. Please retry after 60 seconds.",
             "status": 429
         })),
        (status = 500, description = "Unexpected internal failure (opaque legacy body).",
         example = json!({ "error": "Oops! Something went wrong." })),
    )
)]
pub(crate) async fn get_ticket(
    State(state): State<AppState>,
    auth: AuthSubject,
    Path(who): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    check_rate_limit(&state, &auth.subject)?;

    let row = tickets::get_ticket(&state.pool, &who)
        .await?
        .ok_or(AppError::TicketNotFound)?;
    Ok(Json(tickets::status_response(
        &row,
        OffsetDateTime::now_utc(),
    )))
}

/// Validated `POST /api/v1/dim-ticket` body.
#[derive(Debug)]
struct CreateRequest {
    who: String,
    dim: Dim,
}

/// The zod v4 message for every failed `dim` value — missing, null, wrong
/// type, or an unknown literal all report the same option list.
const INVALID_DIM_OPTION: &str = "Invalid option: expected one of \"Game\"|\"ProofOfInk\"";

/// Validate the request body against the frozen schema (`who`: non-empty
/// string, `dim`: `Game | ProofOfInk`), reproducing the legacy zod v4
/// messages and pointers. Deeper SS58 validity is deliberately NOT checked
/// here — see the module docs.
fn validate_body(body: &serde_json::Value) -> Result<CreateRequest, Vec<FieldError>> {
    if !body.is_object() {
        return Err(vec![FieldError {
            detail: zod::invalid_input("object", zod::type_name(body)),
            pointer: "#/".to_string(),
        }]);
    }

    let mut errors = Vec::new();

    let who = match body.get("who") {
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                errors.push(FieldError {
                    detail: zod::TOO_SMALL_MIN1.to_string(),
                    pointer: "#/who".to_string(),
                });
            }
            Some(s.clone())
        }
        other => {
            errors.push(FieldError {
                detail: zod::invalid_input("string", zod::received_name(other)),
                pointer: "#/who".to_string(),
            });
            None
        }
    };

    let dim = match body.get("dim") {
        Some(serde_json::Value::String(s)) => Dim::from_str(s).ok(),
        _ => None,
    };
    if dim.is_none() {
        errors.push(FieldError {
            detail: INVALID_DIM_OPTION.to_string(),
            pointer: "#/dim".to_string(),
        });
    }

    match (who, dim) {
        (Some(who), Some(dim)) if errors.is_empty() => Ok(CreateRequest { who, dim }),
        _ => Err(errors),
    }
}

/// Request a DIM ticket for an address (JWT-gated).
#[utoipa::path(
    post,
    path = "/api/v1/dim-ticket",
    tag = "DIM Tickets",
    security(("bearer_jwt" = [])),
    request_body = crate::openapi::CreateTicketRequest,
    responses(
        (status = 200, description = "Ticket accepted as `PENDING`; the single-instance chain \
writer registers it on People Chain asynchronously (poll the status route).",
         body = crate::openapi::CreateTicketResponse),
        (status = 400, description = "Body validation failed (RFC 9457 problem details with \
per-field `errors`); or plain-text `Malformed JSON in request body` when the body is not JSON. \
Note SS58 validity is NOT checked here — a well-formed but invalid address is a 500 (legacy \
quirk).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/invalid-body-property-value",
             "title": "Invalid Body Property Value",
             "detail": "The request body contains an invalid body property value.",
             "status": 400,
             "errors": [{ "detail": "Invalid input: expected string, received undefined", "pointer": "#/who" }]
         })),
        (status = 401, description = "Missing/malformed `Authorization` header or failed token \
verification (RFC 9457 problem details).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/unauthorized",
             "title": "Invalid Token",
             "detail": "Token verification failed. The token may be expired or malformed.",
             "status": 401
         })),
        (status = 409, description = "The address already holds a ticket.",
         example = json!({ "error": "Ticket already exists" })),
        (status = 422, description = "The inviter's on-chain invite quota is exhausted.",
         example = json!({ "error": "No available invites" })),
        (status = 429, description = "Per-subject rate limit exceeded (RFC 9457 problem \
details, with `Retry-After`).",
         example = json!({
             "type": "https://problems-registry.smartbear.com/too-many-requests",
             "title": "Too Many Requests",
             "detail": "Rate limit exceeded. Please retry after 60 seconds.",
             "status": 429
         })),
        (status = 500, description = "Unexpected internal failure (opaque legacy body) — \
including an SS58-invalid `who`, which legacy validated in the shell, not the schema.",
         example = json!({ "error": "Oops! Something went wrong." })),
    )
)]
pub(crate) async fn create_ticket(
    State(state): State<AppState>,
    auth: AuthSubject,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Json<serde_json::Value>> {
    let Json(body) = body.map_err(|rejection| match rejection {
        // Legacy quirk: without a JSON content-type, Hono skipped validation
        // entirely and the handler then crashed on the missing body — a 500,
        // not a 400.
        axum::extract::rejection::JsonRejection::MissingJsonContentType(_) => AppError::Internal(
            anyhow::anyhow!("request without JSON content-type (legacy 500)"),
        ),
        // An unparseable body is the Hono plain-text 400.
        _ => AppError::MalformedJson,
    })?;
    let request = validate_body(&body).map_err(AppError::InvalidBody)?;

    check_rate_limit(&state, &auth.subject)?;

    // Legacy guard: the inviter must never invite itself (a defect, not a 4xx).
    if request.who == state.config.inviter_address {
        return Err(AppError::Internal(anyhow::anyhow!(
            "inviter matches ticket: {}",
            request.who
        )));
    }

    // Legacy quirk: SS58 depth-validation failed in the shell, whose typed
    // error fell through to the global hook — a bad address is a 500, not 400.
    subxt::utils::AccountId32::from_str(&request.who)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("invalid SS58 address for who: {e}")))?;

    let available = tokio::time::timeout(
        state.config.quota_timeout,
        state
            .chain
            .available_invites(request.dim, &state.config.inviter_address),
    )
    .await
    .context("quota check timed out")?
    .context("quota check failed")?;

    if available == 0 {
        tracing::warn!(
            dim = request.dim.as_str(),
            available,
            "DIM ticket quota exceeded"
        );
        return Err(AppError::NoAvailableInvites);
    }

    let row = tickets::insert_ticket(
        &state.pool,
        &request.who,
        &state.config.inviter_address,
        state.config.network,
        request.dim,
    )
    .await?
    .ok_or(AppError::TicketAlreadyExists)?;

    tracing::info!(
        dim = request.dim.as_str(),
        network = state.config.network.as_str(),
        "DIM ticket created"
    );
    Ok(Json(tickets::create_response(
        &row,
        OffsetDateTime::now_utc(),
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_valid_body() {
        let body =
            json!({ "who": "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty", "dim": "Game" });
        let parsed = validate_body(&body).expect("valid");
        assert_eq!(parsed.dim, Dim::Game);
        assert!(parsed.who.starts_with("5FHneW"));
    }

    #[test]
    fn rejects_missing_fields_with_pointers() {
        let errors = validate_body(&json!({})).unwrap_err();
        let pointers: Vec<&str> = errors.iter().map(|e| e.pointer.as_str()).collect();
        assert_eq!(pointers, vec!["#/who", "#/dim"]);
        assert_eq!(
            errors[0].detail,
            "Invalid input: expected string, received undefined"
        );
        assert_eq!(errors[1].detail, INVALID_DIM_OPTION);
    }

    #[test]
    fn rejects_empty_who_and_unknown_dim() {
        let errors = validate_body(&json!({ "who": "", "dim": "Chess" })).unwrap_err();
        let details: Vec<&str> = errors.iter().map(|e| e.detail.as_str()).collect();
        assert_eq!(
            details,
            vec![
                "Too small: expected string to have >=1 characters",
                INVALID_DIM_OPTION,
            ]
        );
    }

    #[test]
    fn rejects_wrong_types() {
        let errors = validate_body(&json!({ "who": 5, "dim": 7 })).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors[0].detail,
            "Invalid input: expected string, received number"
        );
        assert_eq!(errors[1].detail, INVALID_DIM_OPTION);
    }

    #[test]
    fn rejects_non_object_bodies_at_the_root_pointer() {
        let errors = validate_body(&json!([1, 2])).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "#/");
        assert_eq!(
            errors[0].detail,
            "Invalid input: expected object, received array"
        );
    }
}
