//! Android attestation CRL: fetch Google's revocation list
//! (`https://android.googleapis.com/attestation/status`) and cache it.
//!
//! The response is JSON `{ "entries": { "<serial>": { "status": ... } } }`;
//! any listed serial is treated as revoked regardless of status/reason (the
//! legacy backend does the same). Refreshes are best-effort: while a snapshot
//! exists, a failed refresh serves the previous snapshot — but only up to
//! `max_stale`. Past that bound the snapshot is refused (surfaced as a hard-mode
//! `503`), so a compromised, since-revoked key can't stay accepted through a
//! long CRL outage. Bounds the legacy "serve stale forever" behavior to the
//! spec's 1-hour maximum staleness (platform-attestation SC-004).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// The CRL could not be fetched or parsed and no snapshot exists.
#[derive(Debug, thiserror::Error)]
pub enum CrlError {
    #[error("attestation CRL fetch failed: {0}")]
    Fetch(String),
    #[error("attestation CRL parse failed: {0}")]
    Parse(String),
}

/// Wire shape of Google's attestation status list.
#[derive(Deserialize)]
struct CrlResponse {
    entries: std::collections::HashMap<String, serde_json::Value>,
}

/// Normalize CRL entry keys: lowercase, so hex serials compare case-insensitively.
fn normalize(entries: std::collections::HashMap<String, serde_json::Value>) -> HashSet<String> {
    entries.into_keys().map(|k| k.to_lowercase()).collect()
}

/// Parse a CRL response body into the revoked-serial set.
pub fn parse_crl(body: &[u8]) -> Result<HashSet<String>, CrlError> {
    let response: CrlResponse =
        serde_json::from_slice(body).map_err(|e| CrlError::Parse(e.to_string()))?;
    Ok(normalize(response.entries))
}

/// A fetched snapshot and when it was taken.
struct Snapshot {
    fetched_at: Instant,
    serials: Arc<HashSet<String>>,
}

/// Cheap-to-clone caching fetcher for the attestation CRL.
#[derive(Clone)]
pub struct CrlCache {
    inner: Arc<Inner>,
}

struct Inner {
    url: String,
    ttl: Duration,
    max_stale: Duration,
    http: reqwest::Client,
    cache: tokio::sync::Mutex<Option<Snapshot>>,
}

/// Outbound timeout for the CRL fetch. The fetch runs while the cache mutex is
/// held, so an unbounded stall would pin every Android attestation waiting on
/// the refresh.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

impl CrlCache {
    /// Create a cache that refetches `url` at most every `ttl` and never serves
    /// a snapshot older than `max_stale` (refusing it once that bound is passed).
    pub fn new(url: String, ttl: Duration, max_stale: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                url,
                ttl,
                max_stale,
                http: reqwest::Client::builder()
                    .timeout(FETCH_TIMEOUT)
                    .build()
                    .expect("reqwest client with a static timeout builds"),
                cache: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// Return the revoked-serial set, fetching or refreshing as needed.
    pub async fn revoked_serials(&self) -> Result<Arc<HashSet<String>>, CrlError> {
        let mut cache = self.inner.cache.lock().await;
        if let Some(snapshot) = cache.as_ref() {
            // Serve the cached set only while it is both fresh (within `ttl`) and
            // within `max_stale`; checking the bound here keeps it honest even
            // if `ttl` is ever configured larger than `max_stale`.
            let age = snapshot.fetched_at.elapsed();
            if age < self.inner.ttl && within_max_stale(age, self.inner.max_stale) {
                return Ok(Arc::clone(&snapshot.serials));
            }
        }

        match self.fetch().await {
            Ok(serials) => {
                let serials = Arc::new(serials);
                *cache = Some(Snapshot {
                    fetched_at: Instant::now(),
                    serials: Arc::clone(&serials),
                });
                Ok(serials)
            }
            // Stale-but-present beats unavailable, but only within `max_stale`.
            // Past that bound the snapshot is refused so a since-revoked key
            // can't stay accepted through a long outage (spec SC-004).
            Err(err) => match cache.as_ref() {
                Some(snapshot) => {
                    let age = snapshot.fetched_at.elapsed();
                    if within_max_stale(age, self.inner.max_stale) {
                        tracing::warn!(
                            error = %err,
                            age_secs = age.as_secs(),
                            "attestation CRL refresh failed; serving stale snapshot within max-stale bound"
                        );
                        Ok(Arc::clone(&snapshot.serials))
                    } else {
                        tracing::error!(
                            error = %err,
                            age_secs = age.as_secs(),
                            "attestation CRL snapshot exceeded max-stale bound; refusing"
                        );
                        Err(err)
                    }
                }
                None => Err(err),
            },
        }
    }

    async fn fetch(&self) -> Result<HashSet<String>, CrlError> {
        let response = self
            .inner
            .http
            .get(&self.inner.url)
            .send()
            .await
            .map_err(|e| CrlError::Fetch(e.to_string()))?
            .error_for_status()
            .map_err(|e| CrlError::Fetch(e.to_string()))?;
        let body = response
            .bytes()
            .await
            .map_err(|e| CrlError::Fetch(e.to_string()))?;
        parse_crl(&body)
    }
}

/// Whether a snapshot of the given `age` may still be served when a refresh
/// fails. The bound is inclusive: a snapshot exactly at `max_stale` is allowed.
fn within_max_stale(age: Duration, max_stale: Duration) -> bool {
    age <= max_stale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_crl_lowercases_the_google_status_shape() {
        // Shape of https://android.googleapis.com/attestation/status entries.
        let body = br#"{
            "entries": {
                "6681152659205225093": { "status": "REVOKED", "reason": "KEY_COMPROMISE" },
                "8350192447815228107": { "status": "SUSPENDED", "reason": "SOFTWARE_FLAW" },
                "ABCDef0123": { "status": "REVOKED" }
            }
        }"#;
        let serials = parse_crl(body).expect("parses");
        assert_eq!(serials.len(), 3);
        assert!(serials.contains("6681152659205225093"));
        assert!(serials.contains("abcdef0123"));

        assert!(parse_crl(b"not json").is_err());
        assert!(parse_crl(b"{}").is_err());
    }

    #[test]
    fn max_stale_bound_is_inclusive() {
        let max = Duration::from_secs(3_600);
        assert!(within_max_stale(Duration::from_secs(0), max));
        assert!(within_max_stale(Duration::from_secs(1_800), max));
        // Exactly at the bound is still served; one second past is refused.
        assert!(within_max_stale(max, max));
        assert!(!within_max_stale(Duration::from_secs(3_601), max));
    }
}
