//! HTTP middleware: a minimal per-route rate limiter.
//!
//! Request tracing/access logging is handled by `tower_http::trace::TraceLayer`
//! in [`crate::http::routes`], not here.
//!
//! The rate limiter is the V1 anti-abuse backstop on the **unauthenticated**
//! auth handshake (postmortem-driven). It is a per-instance fixed window keyed
//! on route + client IP — deliberately simple; Cloudflare edge rules and a
//! per-JWT limiter are Post-V1. Keying on IP is acceptable here only because
//! these routes are pre-authentication; authenticated routes will key on the
//! JWT subject (a whole venue shares one IP).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use super::error::AppError;
use super::state::AppState;

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

/// Reject requests over the per-route/per-IP limit.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let key = format!("{}:{}", req.uri().path(), client_ip(&req));
    if !state.limiter.allow(&key) {
        return AppError::RateLimited.into_response();
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
