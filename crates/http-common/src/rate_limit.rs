//! Per-subject rate limiting for authenticated routes.
//!
//! Keyed on the **JWT subject**, never the client IP: a whole venue shares one
//! public IP under CGNAT, so a per-IP bucket on an authenticated route is a
//! self-inflicted DoS (the legacy shared-nat postmortem). Fixed window,
//! per-instance — the same deliberate simplicity as identity-service's auth
//! limiter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

    /// The window length in whole seconds (the `Retry-After` hint).
    pub fn window_secs(&self) -> u64 {
        self.window.as_secs()
    }
}
