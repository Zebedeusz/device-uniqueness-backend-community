//! iOS Apple Push Notification service (APNs) provider.
//!
//! Mirrors the shipping backend's flat push: top-level `pushId`/`message` plus an
//! `aps` dict (alert sets `alert.title` + `mutable-content`; VoIP sends empty
//! `aps`, the `.voip` topic suffix, and zero expiry). Auth is a cached ES256
//! provider token from the `.p8`; transport is HTTP/2. Config is optional — absent
//! `APNS_*` leaves iOS on the [`UnconfiguredProvider`](super::UnconfiguredProvider).

use std::sync::Mutex;
use std::time::Duration;

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::{ProviderError, PushOutcome, PushProvider, PushRequest, SendFuture};
use crate::config::{required, ConfigError};
use crate::notify::PushError;

/// Alert title shown on non-VoIP pushes (matches the shipping backend).
const ALERT_TITLE: &str = "Polkadot";
/// Non-VoIP push expiry window: APNs stops retrying after this (seconds).
const EXPIRY_DEFAULT_SECONDS: i64 = 3600;
/// VoIP topic suffix appended to the base topic.
const VOIP_TOPIC_SUFFIX: &str = ".voip";
/// Regenerate the provider token once it is older than this (Apple allows <1h).
const TOKEN_REFRESH_SECS: i64 = 3000;
/// Per-request APNs timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Which APNs endpoint to target. The label matches the legacy `environment`
/// field surfaced in error responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsEnvironment {
    /// `api.push.apple.com`.
    Production,
    /// `api.sandbox.push.apple.com` (labelled `development`).
    Development,
}

impl ApnsEnvironment {
    fn host(self) -> &'static str {
        match self {
            ApnsEnvironment::Production => "api.push.apple.com",
            ApnsEnvironment::Development => "api.sandbox.push.apple.com",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ApnsEnvironment::Production => "production",
            ApnsEnvironment::Development => "development",
        }
    }

    fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "production" | "prod" => Ok(ApnsEnvironment::Production),
            "development" | "dev" | "sandbox" => Ok(ApnsEnvironment::Development),
            other => Err(ConfigError::Invalid {
                key: "APNS_ENVIRONMENT",
                reason: format!("must be production or development, got {other:?}"),
            }),
        }
    }
}

/// Validated APNs configuration (all fail-fast; no defaults for secrets).
#[derive(Clone)]
pub struct ApnsConfig {
    /// `.p8` PKCS#8 EC private key contents (PEM).
    pub private_key: String,
    /// APNs auth key id (the `.p8`'s key id).
    pub key_id: String,
    /// Apple developer team id.
    pub team_id: String,
    /// Base topic (the app bundle id); the VoIP topic derives from it.
    pub topic: String,
    /// Which APNs endpoint to target.
    pub environment: ApnsEnvironment,
}

/// Hand-written so the `.p8` private key never reaches logs, spans, or errors.
impl std::fmt::Debug for ApnsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApnsConfig")
            .field("private_key", &"<redacted>")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .field("topic", &self.topic)
            .field("environment", &self.environment)
            .finish()
    }
}

impl ApnsConfig {
    /// Read APNs config from the environment. `APNS_PRIVATE_KEY` (inline PEM)
    /// takes precedence over `APNS_PRIVATE_KEY_FILE` (a path). No key configured
    /// returns `Ok(None)` (the unconfigured stub); once a key is present, every
    /// other field is required.
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        let inline = std::env::var("APNS_PRIVATE_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let key = match inline {
            Some(key) => Some(key),
            None => match std::env::var("APNS_PRIVATE_KEY_FILE")
                .ok()
                .filter(|path| !path.trim().is_empty())
            {
                Some(path) => read_key_file(path.trim())?,
                None => None,
            },
        };
        Self::from_getter(|name| match name {
            "APNS_PRIVATE_KEY" => key.clone(),
            _ => std::env::var(name).ok(),
        })
    }

    /// Read APNs config through a caller-provided lookup (testable).
    pub fn from_getter<F>(get: F) -> Result<Option<Self>, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let Some(private_key) = get("APNS_PRIVATE_KEY").filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            private_key,
            key_id: required(&get, "APNS_KEY_ID")?,
            team_id: required(&get, "APNS_TEAM_ID")?,
            topic: required(&get, "APNS_TOPIC")?,
            environment: ApnsEnvironment::parse(&required(&get, "APNS_ENVIRONMENT")?)?,
        }))
    }
}

/// Read the `.p8` PEM at a configured path. The compose mount defaults to
/// `/dev/null` (a char device) when no key is set — that sentinel reads as
/// unconfigured (`Ok(None)`). Empty, odd, or missing/unreadable is a hard error,
/// so a truncated key or a typo can't silently disable APNs.
fn read_key_file(path: &str) -> Result<Option<String>, ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        key: "APNS_PRIVATE_KEY_FILE",
        reason,
    };
    let meta =
        std::fs::metadata(path).map_err(|error| invalid(format!("cannot read {path}: {error}")))?;
    #[cfg(unix)]
    if std::os::unix::fs::FileTypeExt::is_char_device(&meta.file_type()) {
        return Ok(None);
    }
    if !meta.is_file() {
        return Err(invalid(format!("{path} is not a regular file")));
    }
    let pem = std::fs::read_to_string(path)
        .map_err(|error| invalid(format!("cannot read {path}: {error}")))?;
    if pem.trim().is_empty() {
        return Err(invalid(format!("{path} is empty")));
    }
    Ok(Some(pem))
}

/// Why the APNs provider could not be initialized.
#[derive(Debug, thiserror::Error)]
pub enum ApnsInitError {
    /// The configured `.p8` was not a usable EC private key.
    #[error("invalid APNs private key: {0}")]
    InvalidPrivateKey(#[source] jsonwebtoken::errors::Error),
    /// The HTTP client could not be built.
    #[error("could not build HTTP client: {0}")]
    HttpClient(#[source] reqwest::Error),
}

#[derive(Serialize)]
struct ProviderClaims<'a> {
    iss: &'a str,
    iat: i64,
}

struct CachedToken {
    token: String,
    issued_at: i64,
}

/// Token-authenticated APNs HTTP/2 push provider.
pub struct ApnsProvider {
    client: reqwest::Client,
    config: ApnsConfig,
    encoding_key: EncodingKey,
    token: Mutex<Option<CachedToken>>,
}

impl ApnsProvider {
    /// Build a provider from validated config (parses the key, builds the client).
    pub fn new(config: ApnsConfig) -> Result<Self, ApnsInitError> {
        let encoding_key = EncodingKey::from_ec_pem(config.private_key.as_bytes())
            .map_err(ApnsInitError::InvalidPrivateKey)?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(ApnsInitError::HttpClient)?;
        Ok(Self {
            client,
            config,
            encoding_key,
            token: Mutex::new(None),
        })
    }

    /// Return a cached provider token, minting a fresh one when stale.
    fn provider_token(&self) -> Result<String, ProviderError> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let mut guard = self.token.lock().expect("token lock");
        if let Some(cached) = guard.as_ref() {
            if now - cached.issued_at < TOKEN_REFRESH_SECS {
                return Ok(cached.token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.config.key_id.clone());
        let claims = ProviderClaims {
            iss: &self.config.team_id,
            iat: now,
        };
        let token = jsonwebtoken::encode(&header, &claims, &self.encoding_key)
            .map_err(|error| ProviderError::Delivery(format!("apns provider token: {error}")))?;
        *guard = Some(CachedToken {
            token: token.clone(),
            issued_at: now,
        });
        Ok(token)
    }

    async fn deliver(&self, request: &PushRequest) -> Result<PushOutcome, ProviderError> {
        let token = self.provider_token()?;
        let voip = request.voip.unwrap_or(false);
        let base_topic = request.topic.as_deref().unwrap_or(&self.config.topic);
        let apns_topic = format_topic(base_topic, voip);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let expiry = if voip {
            0
        } else {
            now + EXPIRY_DEFAULT_SECONDS
        };
        let payload = build_payload(&request.push_id, &request.message, voip);
        let url = format!(
            "https://{}/3/device/{}",
            self.config.environment.host(),
            request.device_token
        );

        let response = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {token}"))
            .header("apns-topic", &apns_topic)
            .header("apns-push-type", if voip { "voip" } else { "alert" })
            .header("apns-priority", "10")
            .header("apns-expiration", expiry.to_string())
            .json(&payload)
            .send()
            .await
            .map_err(|error| ProviderError::Delivery(format!("apns request failed: {error}")))?;

        let status = response.status();
        let apns_id = response
            .headers()
            .get("apns-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if status.is_success() {
            debug!(topic = %apns_topic, environment = self.config.environment.label(), apns_id = apns_id.as_deref().unwrap_or("-"), "APNs push accepted");
            // Legacy shape: APNs success reports counts only, never a messageId.
            return Ok(apns_success());
        }

        let body = response.text().await.unwrap_or_default();
        let reason = parse_reason(&body);
        warn!(
            topic = %apns_topic,
            environment = self.config.environment.label(),
            status = status.as_u16(),
            reason = reason.as_deref().unwrap_or("-"),
            "APNs push rejected"
        );
        // Legacy: a terminal token error (the device never delivered and the only
        // failure carries a terminal reason) is promoted to the generic
        // `200 success:false` route fallback; any other rejection returns the
        // structured per-device failure.
        match reason.as_deref().and_then(terminal_reason) {
            Some(terminal) => Err(ProviderError::Delivery(terminal.to_string())),
            None => Ok(failure_outcome(
                &request.device_token,
                self.config.environment.label(),
                status.as_u16(),
                reason,
            )),
        }
    }
}

impl PushProvider for ApnsProvider {
    fn send<'a>(&'a self, request: &'a PushRequest) -> SendFuture<'a> {
        Box::pin(async move { self.deliver(request).await })
    }
}

/// Append the `.voip` suffix for VoIP pushes (idempotently); alert topics are
/// used verbatim.
fn format_topic(topic: &str, voip: bool) -> String {
    if !voip || topic.ends_with(VOIP_TOPIC_SUFFIX) {
        return topic.to_string();
    }
    format!("{topic}{VOIP_TOPIC_SUFFIX}")
}

/// Build the flat APNs JSON payload, matching the shipping backend byte-for-byte.
fn build_payload(push_id: &str, message: &str, voip: bool) -> Value {
    let aps = if voip {
        json!({})
    } else {
        json!({ "alert": { "title": ALERT_TITLE }, "mutable-content": 1 })
    };
    json!({ "pushId": push_id, "message": message, "aps": aps })
}

/// A single accepted APNs push: one sent, none failed, and — matching the
/// legacy adapter — no `messageId` (Apple's `apns-id` is logged, not returned).
fn apns_success() -> PushOutcome {
    PushOutcome {
        success: true,
        sent: Some(1),
        failed: Some(0),
        message_id: None,
        errors: None,
    }
}

/// The legacy terminal APNs reasons: a rejection carrying one of these means the
/// token is permanently invalid, so the relay reports the generic failure body
/// rather than the structured per-device error.
fn terminal_reason(reason: &str) -> Option<&'static str> {
    match reason {
        "Unregistered" | "ExpiredToken" => Some("token_unregistered"),
        "BadDeviceToken" | "DeviceTokenNotForTopic" => Some("token_invalid"),
        _ => None,
    }
}

fn failure_outcome(
    device_token: &str,
    environment: &str,
    status: u16,
    reason: Option<String>,
) -> PushOutcome {
    PushOutcome {
        success: false,
        sent: Some(0),
        failed: Some(1),
        message_id: None,
        errors: Some(vec![PushError {
            device: device_token.to_string(),
            environment: Some(environment.to_string()),
            status: Some(json!(status)),
            response: reason.map(|reason| json!({ "reason": reason })),
        }]),
    }
}

/// Extract APNs `{"reason": "..."}` from an error body, if present.
fn parse_reason(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("reason")?.as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::collections::HashMap;

    // Throwaway P-256 PKCS#8 key for tests only — NOT a real Apple key.
    const TEST_P8: &str = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgX9sV73EVkyqpDvfx\nvhX57WH3j3jye7saQGuS7OfPBZGhRANCAAQABHXKAeCm1wfSBNqsGBZSElcwqrvM\n/ZKAaIFLzOY03fbF+uvMyZ17XN1+gdf2ibbYBasr21Oi65n1vIzkYQU/\n-----END PRIVATE KEY-----\n";

    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn config(pairs: &[(&str, &str)]) -> Result<Option<ApnsConfig>, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        ApnsConfig::from_getter(|key| map.get(key).cloned())
    }

    fn valid_config() -> ApnsConfig {
        ApnsConfig {
            private_key: TEST_P8.to_string(),
            key_id: "73L6WWUQ3F".to_string(),
            team_id: "P2PX3JU8FT".to_string(),
            topic: "io.parity.brevity".to_string(),
            environment: ApnsEnvironment::Development,
        }
    }

    #[test]
    fn topic_gets_voip_suffix_only_for_voip_and_idempotently() {
        assert_eq!(
            format_topic("io.parity.brevity", false),
            "io.parity.brevity"
        );
        assert_eq!(
            format_topic("io.parity.brevity", true),
            "io.parity.brevity.voip"
        );
        assert_eq!(
            format_topic("io.parity.brevity.voip", true),
            "io.parity.brevity.voip"
        );
    }

    #[test]
    fn alert_payload_matches_legacy_shape() {
        let payload = build_payload("pid", "0xdead", false);
        assert_eq!(payload["pushId"], "pid");
        assert_eq!(payload["message"], "0xdead");
        assert_eq!(payload["aps"]["alert"]["title"], ALERT_TITLE);
        assert_eq!(payload["aps"]["mutable-content"], 1);
    }

    #[test]
    fn voip_payload_has_empty_aps_and_no_alert() {
        let payload = build_payload("pid", "0xdead", true);
        assert_eq!(payload["pushId"], "pid");
        assert!(payload["aps"].as_object().unwrap().is_empty());
        assert!(payload["aps"]["alert"].is_null());
    }

    #[test]
    fn environment_parses_and_maps_host_and_label() {
        assert_eq!(
            ApnsEnvironment::parse("production").unwrap(),
            ApnsEnvironment::Production
        );
        assert_eq!(
            ApnsEnvironment::parse("SANDBOX").unwrap(),
            ApnsEnvironment::Development
        );
        assert!(ApnsEnvironment::parse("staging").is_err());
        assert_eq!(ApnsEnvironment::Production.host(), "api.push.apple.com");
        assert_eq!(
            ApnsEnvironment::Development.host(),
            "api.sandbox.push.apple.com"
        );
        assert_eq!(ApnsEnvironment::Development.label(), "development");
    }

    #[test]
    fn config_is_none_when_unconfigured_and_required_when_partial() {
        assert!(config(&[]).unwrap().is_none());
        assert!(matches!(
            config(&[("APNS_PRIVATE_KEY", TEST_P8)]).expect_err("partial config"),
            ConfigError::Missing("APNS_KEY_ID")
        ));
        let full = config(&[
            ("APNS_PRIVATE_KEY", TEST_P8),
            ("APNS_KEY_ID", "73L6WWUQ3F"),
            ("APNS_TEAM_ID", "P2PX3JU8FT"),
            ("APNS_TOPIC", "io.parity.brevity"),
            ("APNS_ENVIRONMENT", "development"),
        ])
        .unwrap()
        .expect("configured");
        assert_eq!(full.environment, ApnsEnvironment::Development);
        assert_eq!(full.topic, "io.parity.brevity");
    }

    #[test]
    fn key_file_reads_content_rejects_empty_and_missing() {
        assert!(read_key_file("/no/such/apns.p8").is_err());

        let path = std::env::temp_dir().join(format!("apns-empty-{}.p8", std::process::id()));
        std::fs::write(&path, "  \n").expect("write temp key");
        assert!(read_key_file(path.to_str().unwrap()).is_err());
        std::fs::write(&path, TEST_P8).expect("write temp key");
        assert_eq!(
            read_key_file(path.to_str().unwrap()).unwrap().as_deref(),
            Some(TEST_P8)
        );
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn dev_null_sentinel_reads_as_unconfigured() {
        // The compose default mounts /dev/null when no key is set.
        assert_eq!(read_key_file("/dev/null").unwrap(), None);
    }

    #[test]
    fn provider_token_is_es256_with_kid_and_team_issuer() {
        let provider = ApnsProvider::new(valid_config()).expect("valid key");
        let token = provider.provider_token().expect("token");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWS has three segments");

        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "73L6WWUQ3F");

        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "P2PX3JU8FT");
        assert!(claims["iat"].as_i64().unwrap() > 0);

        // Second call within the refresh window returns the cached token.
        assert_eq!(provider.provider_token().unwrap(), token);
    }

    #[test]
    fn success_reports_counts_without_a_message_id() {
        let outcome = apns_success();
        assert!(outcome.success);
        assert_eq!(outcome.sent, Some(1));
        assert_eq!(outcome.failed, Some(0));
        assert_eq!(outcome.message_id, None);
    }

    #[test]
    fn terminal_reasons_are_classified_and_others_are_not() {
        assert_eq!(terminal_reason("Unregistered"), Some("token_unregistered"));
        assert_eq!(terminal_reason("ExpiredToken"), Some("token_unregistered"));
        assert_eq!(terminal_reason("BadDeviceToken"), Some("token_invalid"));
        assert_eq!(
            terminal_reason("DeviceTokenNotForTopic"),
            Some("token_invalid")
        );
        // Non-terminal (or unknown) reasons keep the structured failure body.
        assert_eq!(terminal_reason("TooManyRequests"), None);
        assert_eq!(terminal_reason("PayloadTooLarge"), None);
    }

    #[test]
    fn failure_outcome_carries_environment_status_and_reason() {
        // A non-terminal reason keeps the structured per-device failure body.
        let outcome = failure_outcome(
            "abc",
            "development",
            429,
            Some("TooManyRequests".to_string()),
        );
        assert!(!outcome.success);
        assert_eq!(outcome.failed, Some(1));
        let error = &outcome.errors.as_ref().unwrap()[0];
        assert_eq!(error.device, "abc");
        assert_eq!(error.environment.as_deref(), Some("development"));
        assert_eq!(error.status, Some(json!(429)));
        assert_eq!(error.response, Some(json!({ "reason": "TooManyRequests" })));
    }

    #[test]
    fn parse_reason_reads_apns_error_body() {
        assert_eq!(
            parse_reason(r#"{"reason":"BadDeviceToken"}"#),
            Some("BadDeviceToken".to_string())
        );
        assert_eq!(parse_reason(""), None);
    }
}
