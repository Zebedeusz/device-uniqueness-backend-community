//! Push provider boundary: the [`PushProvider`] trait, the flat request/outcome
//! types, and two fakes — [`RecordingProvider`] (contract tests) and
//! [`UnconfiguredProvider`] (unwired platform slots, so an un-provisioned relay
//! reports provider failure rather than a false success). iOS APNs is in [`apns`],
//! Android FCM in [`fcm`].

pub mod apns;
pub mod fcm;

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::notify::PushError;

/// A boxed, `Send` future — keeps [`PushProvider`] object-safe without pulling in
/// an async-trait dependency.
pub type SendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PushOutcome, ProviderError>> + Send + 'a>>;

/// The flat push payload handed to a provider, mirroring the legacy request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    /// Recipient device token (APNs hex or FCM token).
    pub device_token: String,
    /// Opaque push id echoed to the client app.
    pub push_id: String,
    /// Hex-encoded, already-encrypted message body.
    pub message: String,
    /// Optional APNs topic override, derived from the request `bundlerId`.
    pub topic: Option<String>,
    /// iOS VoIP push flag.
    pub voip: Option<bool>,
}

/// A provider's successful send result (platform is attached by the relay).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushOutcome {
    /// Whether the provider accepted the push.
    pub success: bool,
    /// Number of notifications sent, when the provider reports it.
    pub sent: Option<u32>,
    /// Number of failed notifications, when the provider reports it.
    pub failed: Option<u32>,
    /// Provider message id, when returned (FCM).
    pub message_id: Option<String>,
    /// Per-device errors, when partial failures occurred.
    pub errors: Option<Vec<PushError>>,
}

/// A provider send failure (network/credentials/etc.).
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    /// No real provider is wired for this platform yet.
    #[error("push provider not configured")]
    Unconfigured,
    /// The provider rejected or could not deliver the push.
    #[error("{0}")]
    Delivery(String),
}

/// Sends a flat push payload to a concrete push service.
pub trait PushProvider: Send + Sync {
    /// Deliver `request`, resolving to an outcome or a provider error.
    fn send<'a>(&'a self, request: &'a PushRequest) -> SendFuture<'a>;
}

/// Captures the last payload it received and returns a preset outcome.
///
/// Used by contract tests to assert the exact flat payload the relay forwards.
pub struct RecordingProvider {
    outcome: Result<PushOutcome, ProviderError>,
    last_request: Mutex<Option<PushRequest>>,
}

impl RecordingProvider {
    /// Build a recorder that always resolves to `outcome`.
    pub fn new(outcome: Result<PushOutcome, ProviderError>) -> Self {
        Self {
            outcome,
            last_request: Mutex::new(None),
        }
    }

    /// The most recent payload the relay forwarded, if any.
    pub fn last_request(&self) -> Option<PushRequest> {
        self.last_request.lock().expect("recorder lock").clone()
    }
}

impl PushProvider for RecordingProvider {
    fn send<'a>(&'a self, request: &'a PushRequest) -> SendFuture<'a> {
        let recorded = request.clone();
        let outcome = self.outcome.clone();
        Box::pin(async move {
            *self.last_request.lock().expect("recorder lock") = Some(recorded);
            outcome
        })
    }
}

/// Reports provider failure for any unwired platform slot.
pub struct UnconfiguredProvider;

impl PushProvider for UnconfiguredProvider {
    fn send<'a>(&'a self, _request: &'a PushRequest) -> SendFuture<'a> {
        Box::pin(async { Err(ProviderError::Unconfigured) })
    }
}
