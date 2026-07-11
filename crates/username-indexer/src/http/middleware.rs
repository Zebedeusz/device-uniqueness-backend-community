//! HTTP middleware: a minimal in-memory rate limiter for public search.
//!
//! Request tracing/access logging is handled by `tower_http::trace::TraceLayer`
//! in [`crate::http::routes`], not here.
//!
//! The rate limiter is a per-instance fixed window keyed on client IP. It is the
//! only anti-abuse backstop on the **public, unauthenticated**
//! `GET /api/v1/usernames/search` route — there is no JWT and no
//! proof-of-compute here, so the key is the best-effort client IP. It is
//! deliberately simple; edge rules (e.g. Cloudflare) remain the outer defence.

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

/// Reject public search requests over the per-IP limit.
///
/// The path is constant (this layer only wraps the search route), so the key is
/// the best-effort client IP alone.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !state.limiter.allow(&client_ip(&req)) {
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
