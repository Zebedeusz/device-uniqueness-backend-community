//! HTTP middleware: the public-read rate limiter and the proof-of-compute gate.
//!
//! Request tracing/access logging is handled by `tower_http::trace::TraceLayer`
//! in [`crate::http::routes`], not here.
//!
//! The rate limiter is a per-instance fixed window keyed on client IP. It is the
//! outermost anti-abuse backstop on the **public**
//! `GET /api/v1/usernames/search` route and does **no** cryptography, so an
//! exhausted bucket cannot be made to burn CPU. Inside it, [`poc_gate`] admits
//! callers that present either a valid identity-service JWT or a solved puzzle
//! (only when `POC_ENABLED`). Edge rules (e.g. Cloudflare) remain the outer
//! defence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use super::error::AppError;
use super::state::AppState;
use crate::poc::Rejection;

/// Fixed-window rate limiter shared across handlers (cheap to clone).
#[derive(Clone)]
pub struct RateLimiter {
    windows: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    limit: u32,
    window: Duration,
}

impl RateLimiter {
    /// Allow `limit` requests per `window` per key.
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            windows: Arc::new(Mutex::new(HashMap::new())),
            limit,
            window,
        }
    }

    /// The window length in whole seconds (the `Retry-After` value).
    pub fn window_secs(&self) -> u64 {
        self.window.as_secs()
    }

    /// Record a hit for `key`; returns `false` when the limit is exceeded.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("rate limiter mutex poisoned");
        let entry = windows.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) > self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// Reject public read requests over the per-IP limit.
///
/// Keyed on the best-effort client IP and deliberately doing no signature work:
/// it must be the cheapest layer on the route so a client that has exhausted its
/// window cannot force JWT verification or puzzle hashing. The 429 renders the
/// frozen legacy problem-details body with `Retry-After`.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !state.limiter.allow(&client_ip(&req)) {
        return AppError::RateLimited {
            retry_after_secs: state.limiter.window_secs(),
        }
        .into_response();
    }
    next.run(req).await
}

/// Best-effort client IP from proxy headers (edge terminates TLS).
fn client_ip(req: &Request) -> String {
    let headers = req.headers();
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .or_else(|| {
            headers
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Admit a public read when the caller presents **either** a valid
/// identity-service JWT **or** a fresh, solved proof-of-compute puzzle.
///
/// Pass-through when the gate is disabled (`state.poc` is `None`), which is the
/// shipping default. A bearer token that fails verification is treated as
/// *anonymous* rather than rejected: search must stay a public route, so this
/// middleware never emits a `401`.
pub async fn poc_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(poc) = state.poc.clone() else {
        return next.run(req).await;
    };

    if bearer_token(&req).is_some_and(|token| poc.jwt().verify(token).is_ok()) {
        return next.run(req).await;
    }

    let Some(header) = req
        .headers()
        .get(crate::poc::solution::HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return AppError::Poc(Rejection::Missing).into_response();
    };

    let solution = match crate::poc::Solution::parse_header(header) {
        Ok(solution) => solution,
        Err(rejection) => return AppError::Poc(rejection).into_response(),
    };

    match poc
        .verify(&state.pool, &solution, crate::poc::now_millis())
        .await
    {
        Ok(Ok(())) => next.run(req).await,
        Ok(Err(rejection)) => AppError::Poc(rejection).into_response(),
        Err(error) => AppError::Internal(error.into()).into_response(),
    }
}

/// The bearer token on the request, if the `Authorization` header carries one.
fn bearer_token(req: &Request) -> Option<&str> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let mut parts = header.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("Bearer"), Some(token), None) if !token.is_empty() => Some(token),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::RateLimiter;

    #[test]
    fn allows_up_to_limit_then_denies() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
    }

    #[test]
    fn tracks_keys_independently() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.allow("1.1.1.1"));
        assert!(!limiter.allow("1.1.1.1"));
        assert!(limiter.allow("2.2.2.2"));
    }
}
