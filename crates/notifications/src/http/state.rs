//! Shared application state.

use std::sync::Arc;

use http_common::RateLimiter;
use jwt_verify::Verifier;

use crate::provider::PushProvider;

/// Cheap-to-clone handles for handlers: the JWT verifier, the two providers, and
/// the per-subject notify rate limiter.
#[derive(Clone)]
pub struct AppState {
    /// Verify-only Ed25519 JWT validator.
    pub verifier: Verifier,
    /// Provider used for iOS (APNs) pushes.
    pub apns: Arc<dyn PushProvider>,
    /// Provider used for Android (FCM) pushes.
    pub fcm: Arc<dyn PushProvider>,
    /// In-memory per-subject rate limiter for `/api/v1/notify`.
    pub limiter: RateLimiter,
}

impl AppState {
    /// Build handler state from a verifier, the two platform providers, and the
    /// notify rate limiter.
    pub fn new(
        verifier: Verifier,
        apns: Arc<dyn PushProvider>,
        fcm: Arc<dyn PushProvider>,
        limiter: RateLimiter,
    ) -> Self {
        Self {
            verifier,
            apns,
            fcm,
            limiter,
        }
    }
}

impl http_common::HasJwtVerifier for AppState {
    fn jwt_verifier(&self) -> &Verifier {
        &self.verifier
    }
}
