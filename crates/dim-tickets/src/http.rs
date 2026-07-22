//! HTTP surface for `dim-tickets-api`: the two frozen legacy routes plus the
//! shared axum primitives (state, errors, extractors, health).
//!
//! Route behavior — including its quirks — is the legacy contract:
//! - `POST /api/v1/dim-ticket`: body-validate (400 problem details) → auth
//!   (401) → per-subject rate limit (429) → guard `who != inviter` (500, the
//!   legacy `die`) → SS58-parse `who` (**500**, not 400 — legacy validated
//!   SS58 in the shell where its typed error fell through to the global error
//!   hook) → on-chain quota (≤0 → 422) → insert (conflict → 409) → 200.
//! - `GET /api/v1/dim-ticket/{who}`: auth → rate limit → lookup (missing →
//!   404) → 200. No SS58 validation at all, exactly like legacy.

pub mod error;
pub mod extract;
pub mod health;
pub mod middleware;
pub mod state;

use std::str::FromStr as _;
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use time::OffsetDateTime;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use self::error::{AppError, AppResult, FieldError};
use self::extract::AuthSubject;
use self::state::AppState;
use crate::tickets::{self, Dim};

/// Per-request timeout; a slow handler yields `408` instead of pinning a connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble the full router: health at root, the frozen `/api/v1/dim-ticket`
/// surface below, wrapped in the production middleware stack.
pub fn routes(state: AppState) -> Router {
    let api = Router::new()
        .route("/", post(create_ticket))
        .route("/{who}", get(get_ticket));

    Router::new()
        .merge(health::router())
        .nest("/api/v1/dim-ticket", api)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_http_span)
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

/// Per-request tracing span carrying the request id, method, and path.
fn make_http_span(request: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %request.method(),
        path = request.uri().path(),
    )
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

/// `GET /api/v1/dim-ticket/{who}` — frozen ticket-status lookup.
async fn get_ticket(
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

/// Validate the request body against the frozen schema (`who`: non-empty
/// string, `dim`: `Game | ProofOfInk`), accumulating legacy-shaped field
/// errors. Deeper SS58 validity is deliberately NOT checked here — see the
/// module docs.
fn validate_body(body: &serde_json::Value) -> Result<CreateRequest, Vec<FieldError>> {
    let mut errors = Vec::new();

    let who = match body.get("who") {
        None | Some(serde_json::Value::Null) => {
            errors.push(FieldError {
                detail: "Required".to_string(),
                pointer: "#/who".to_string(),
            });
            None
        }
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(serde_json::Value::String(_)) => {
            errors.push(FieldError {
                detail: "String must contain at least 1 character(s)".to_string(),
                pointer: "#/who".to_string(),
            });
            None
        }
        Some(_) => {
            errors.push(FieldError {
                detail: "Expected string".to_string(),
                pointer: "#/who".to_string(),
            });
            None
        }
    };

    let dim = match body.get("dim") {
        None | Some(serde_json::Value::Null) => {
            errors.push(FieldError {
                detail: "Required".to_string(),
                pointer: "#/dim".to_string(),
            });
            None
        }
        Some(serde_json::Value::String(s)) => match Dim::from_str(s) {
            Ok(dim) => Some(dim),
            Err(()) => {
                errors.push(FieldError {
                    detail: format!(
                        "Invalid enum value. Expected 'Game' | 'ProofOfInk', received '{s}'"
                    ),
                    pointer: "#/dim".to_string(),
                });
                None
            }
        },
        Some(_) => {
            errors.push(FieldError {
                detail: "Expected 'Game' | 'ProofOfInk'".to_string(),
                pointer: "#/dim".to_string(),
            });
            None
        }
    };

    match (who, dim) {
        (Some(who), Some(dim)) if errors.is_empty() => Ok(CreateRequest { who, dim }),
        _ => Err(errors),
    }
}

/// `POST /api/v1/dim-ticket` — frozen ticket-request route.
async fn create_ticket(
    State(state): State<AppState>,
    auth: AuthSubject,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Json<serde_json::Value>> {
    let Json(body) = body.map_err(|rejection| {
        AppError::InvalidBody(vec![FieldError {
            detail: rejection.body_text(),
            pointer: "#/".to_string(),
        }])
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
        assert!(errors.iter().all(|e| e.detail == "Required"));
    }

    #[test]
    fn rejects_empty_who_and_unknown_dim() {
        let errors = validate_body(&json!({ "who": "", "dim": "Chess" })).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].detail.contains("at least 1 character"));
        assert!(errors[1].detail.contains("'Chess'"));
    }

    #[test]
    fn rejects_wrong_types() {
        let errors = validate_body(&json!({ "who": 5, "dim": 7 })).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].detail, "Expected string");
    }
}
