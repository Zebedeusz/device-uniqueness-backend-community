//! Android Firebase Cloud Messaging (FCM) provider.
//!
//! Mirrors the shipping backend's flat push: an FCM v1 `data` message with
//! `pushType: "chat"`, `pushId`, `message` and `android.priority: "high"`. Auth is
//! the FCM v1 OAuth2 flow — a cached access token obtained by signing an RS256
//! assertion with the service-account key and exchanging it at Google's token
//! endpoint. Config is optional — absent `FCM_SERVICE_ACCOUNT_JSON` leaves Android
//! on the [`UnconfiguredProvider`](super::UnconfiguredProvider).

use std::sync::Mutex;
use std::time::Duration;

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::{ProviderError, PushOutcome, PushProvider, PushRequest, SendFuture};
use crate::config::ConfigError;

const PUSH_TYPE: &str = "chat";
const ANDROID_PRIORITY: &str = "high";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Refresh the access token this many seconds before it actually expires.
const TOKEN_REFRESH_BUFFER_SECS: i64 = 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Validated FCM configuration (from a service-account JSON).
#[derive(Clone)]
pub struct FcmConfig {
    /// Firebase project id (the FCM v1 send path segment).
    pub project_id: String,
    /// Service-account client email (the assertion issuer).
    pub client_email: String,
    /// Service-account RSA private key (PEM).
    pub private_key: String,
}

/// Hand-written so the service-account private key never reaches logs or errors.
impl std::fmt::Debug for FcmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FcmConfig")
            .field("project_id", &self.project_id)
            .field("client_email", &self.client_email)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct ServiceAccount {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    client_email: String,
    #[serde(default)]
    private_key: String,
}

impl FcmConfig {
    /// Read FCM config from the environment.
    ///
    /// Returns `Ok(None)` when Android FCM is not configured (no
    /// `FCM_SERVICE_ACCOUNT_JSON`); when present it must be a valid service-account
    /// JSON with `project_id`, `client_email`, and `private_key`.
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        Self::from_getter(|key| std::env::var(key).ok())
    }

    /// Read FCM config through a caller-provided lookup (testable).
    pub fn from_getter<F>(get: F) -> Result<Option<Self>, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let Some(raw) = get("FCM_SERVICE_ACCOUNT_JSON").filter(|v| !v.trim().is_empty()) else {
            return Ok(None);
        };
        let account: ServiceAccount =
            serde_json::from_str(&raw).map_err(|error| ConfigError::Invalid {
                key: "FCM_SERVICE_ACCOUNT_JSON",
                reason: format!("not valid service-account JSON: {error}"),
            })?;
        if account.project_id.is_empty()
            || account.client_email.is_empty()
            || account.private_key.is_empty()
        {
            return Err(ConfigError::Invalid {
                key: "FCM_SERVICE_ACCOUNT_JSON",
                reason: "missing project_id, client_email, or private_key".to_string(),
            });
        }
        Ok(Some(Self {
            project_id: account.project_id,
            client_email: account.client_email,
            private_key: account.private_key,
        }))
    }
}

/// Why the FCM provider could not be initialized.
#[derive(Debug, thiserror::Error)]
pub enum FcmInitError {
    /// The service-account private key was not a usable RSA key.
    #[error("invalid FCM private key: {0}")]
    InvalidPrivateKey(#[source] jsonwebtoken::errors::Error),
    /// The HTTP client could not be built.
    #[error("could not build HTTP client: {0}")]
    HttpClient(#[source] reqwest::Error),
}

#[derive(Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
}

struct CachedToken {
    token: String,
    expires_at: i64,
}

/// OAuth2-authenticated FCM v1 push provider.
pub struct FcmProvider {
    client: reqwest::Client,
    config: FcmConfig,
    encoding_key: EncodingKey,
    token: Mutex<Option<CachedToken>>,
}

impl FcmProvider {
    /// Build a provider from validated config (parses the key, builds the client).
    pub fn new(config: FcmConfig) -> Result<Self, FcmInitError> {
        let encoding_key = EncodingKey::from_rsa_pem(config.private_key.as_bytes())
            .map_err(FcmInitError::InvalidPrivateKey)?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(FcmInitError::HttpClient)?;
        Ok(Self {
            client,
            config,
            encoding_key,
            token: Mutex::new(None),
        })
    }

    fn cached_token(&self) -> Option<String> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let guard = self.token.lock().expect("token lock");
        guard
            .as_ref()
            .filter(|cached| now < cached.expires_at)
            .map(|cached| cached.token.clone())
    }

    /// Sign the RS256 assertion the token endpoint exchanges for an access token.
    fn assertion(&self, now: i64) -> Result<String, ProviderError> {
        let claims = AssertionClaims {
            iss: &self.config.client_email,
            scope: FCM_SCOPE,
            aud: TOKEN_ENDPOINT,
            iat: now,
            exp: now + 3600,
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.encoding_key)
            .map_err(|error| ProviderError::Delivery(format!("fcm assertion: {error}")))
    }

    /// Return a cached access token, exchanging a fresh assertion when stale.
    async fn access_token(&self) -> Result<String, ProviderError> {
        if let Some(token) = self.cached_token() {
            return Ok(token);
        }
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let assertion = self.assertion(now)?;
        let response = self
            .client
            .post(TOKEN_ENDPOINT)
            .form(&[("grant_type", GRANT_TYPE), ("assertion", &assertion)])
            .send()
            .await
            .map_err(|error| {
                ProviderError::Delivery(format!("fcm token exchange failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Delivery(format!(
                "fcm token exchange rejected ({}): {body}",
                status.as_u16()
            )));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|error| ProviderError::Delivery(format!("fcm token response: {error}")))?;
        let expires_at = now + token.expires_in - TOKEN_REFRESH_BUFFER_SECS;
        *self.token.lock().expect("token lock") = Some(CachedToken {
            token: token.access_token.clone(),
            expires_at,
        });
        Ok(token.access_token)
    }

    async fn deliver(&self, request: &PushRequest) -> Result<PushOutcome, ProviderError> {
        let access_token = self.access_token().await?;
        let body = build_message(&request.device_token, &request.push_id, &request.message);
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.config.project_id
        );

        let response = self
            .client
            .post(&url)
            .bearer_auth(&access_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| ProviderError::Delivery(format!("fcm request failed: {error}")))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            let message_id = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| value.get("name")?.as_str().map(str::to_string));
            debug!(project = %self.config.project_id, "FCM push accepted");
            // Legacy shape: FCM success reports sent + messageId, never `failed`.
            return Ok(fcm_success(message_id));
        }

        warn!(
            project = %self.config.project_id,
            status = status.as_u16(),
            reason = fcm_error_code(&body).as_deref().unwrap_or("-"),
            "FCM push rejected"
        );
        // Legacy: `messaging.send()` rejects on any provider failure, so the
        // route emits the generic `200 success:false` fallback — a returned
        // error here routes the notify handler down that same path.
        Err(ProviderError::Delivery(fcm_error_message(
            &body,
            status.as_u16(),
        )))
    }
}

impl PushProvider for FcmProvider {
    fn send<'a>(&'a self, request: &'a PushRequest) -> SendFuture<'a> {
        Box::pin(async move { self.deliver(request).await })
    }
}

/// Build the FCM v1 message body, matching the shipping backend's flat `data`.
fn build_message(device_token: &str, push_id: &str, message: &str) -> Value {
    json!({
        "message": {
            "token": device_token,
            "data": { "pushType": PUSH_TYPE, "pushId": push_id, "message": message },
            "android": { "priority": ANDROID_PRIORITY },
        }
    })
}

/// A single accepted FCM push: `sent` plus the provider message id, and —
/// matching the legacy adapter — no `failed` field.
fn fcm_success(message_id: Option<String>) -> PushOutcome {
    PushOutcome {
        success: true,
        sent: Some(1),
        failed: None,
        message_id,
        errors: None,
    }
}

/// A concise message for the generic failure body: the FCM v1 `error.message`
/// (or `error.status`), falling back to the HTTP status. Kept short so raw
/// provider diagnostics do not leak into the response.
fn fcm_error_message(body: &str, status: u16) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            let error = value.get("error")?;
            error
                .get("message")
                .or_else(|| error.get("status"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("FCM rejected the push (status {status})"))
}
/// Extract the FCM v1 `errorCode` (or `status`) from an error body, for logging.
fn fcm_error_code(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?;
    error
        .get("details")
        .and_then(|details| details.as_array())
        .and_then(|details| details.iter().find_map(|d| d.get("errorCode")?.as_str()))
        .or_else(|| error.get("status").and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::collections::HashMap;

    // Throwaway 2048-bit RSA PKCS#8 key for tests only — NOT a real key.
    const TEST_RSA: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDIx8ZRNzzE1j7T\nmKphU2VTJj6tsJkkJepRMNrSOFS7TrwmGG3LmgoqMpaKk6mgaASjtgayQy7JkXWS\n+rLjoriA3BiMN7HQR+LbMdz1xTQ4IqROZg9V63iz6LiRaOK1Iy6O72syZu9WFSeC\nKqpbOhVPLOukc80mvYMNv52nWjlE8//9Xr3MDnwuoIS7zGXCpqeRkWTFzlTJT6UJ\nboGOWQmdzVnb420kW5Eh+tAl2VG10NiS53BRtvtKb60FPs6XaE4oYOeO7DquNeDU\n9suyFZTtO2TDZgBlu2wdewTX4IQlK6laWHh0OxI0kyoRr4XkcCciu2Mnm++chFfF\nO+/Do/urAgMBAAECggEAXxH5XcuCAtJPvXG0e5E5kT2ywBu2597sLLWtZ4000dxe\nSsuWcanm1kRN9CAnWcTnLX2rfg1BwkAWJBqBSjhqlNmi5gIZOD5RtOjPb7CJIWuF\n0XmSGWT+KnIvsuqvLoYSLaxAXgVPnrs1kfUR5VCDdpIWojATHBzwSTpKNEwMghEQ\nR/L0E6s5+PGEhW51ROJjkI581uL83khJQIGXQwAimcH0nVrQ9aq7yrUDM83ptSx2\nO34yMA4OKyGwJz931HMmVuKj0BL/3il60kSQpLX5AHncUHNrAXfegSuMI9vYBW6q\nh4W9Gv6ctL60sTtmOiN5MNuKLqKLLyNTKpE1U+HEQQKBgQDvZojpf7A6aIxq4okF\nnYYQYgWvRUOxbmYCIvdTQ6dAHpsvcXHwRL9g88evuoxwrzWE76FKEaiFPjhDYXPa\nAgg3PIcNRFcnMHjJZw84SM3Gwb7JWrWkS2+RKndZHHb/4C9ZES9LFCHDDjQkd8iD\nf46Y1wBoYIGDbzj9rYyRvRMwRQKBgQDWs7cKsNIIt+0vG7Js2tF1CpIh0ugO2k8k\ngri3l6a4MLBMV/gRfk0L51AdxcWsROwk6afX3GZiCoBxjTmjktfIsuNVe/UZ8wDL\ns1Z94sqQJWQ3AmWOroAf6CyLdvlbV1fqu3biAF+ZCcQ/nM4y/NtTIJiBIOKj/voo\nSFqx3XUTLwKBgQDNd/taLLVb7A9YTUW9BA3kUbz/STtoNZBnlQsg85fAeIRIm91m\nkhqPY6unLz0KGdadWe3cXHt+oIA5lJKSMdxLTC+9O0Jx6DBC66ksbY/vXqoYtzne\n4L/In+H/IWchBZCdqRomHgk8GBy4j/YQppIEq8M10l5WKEeJskJLczPc4QKBgECd\nByoNergq+hNiR3khBUYu7zmEqlfF9WlsecCuv/rQlE31b298Th2V+HNtUIb+mv6k\n3uFEr/8zX+JMeRs0FwKMa4QPcPzlN8kV6KKr/QAScK1paDzfYSm5CqbSIEsP5yJ6\nVlW/fQWmfcwTGa8Yj7zdo2fBCNQH6Sr0U28R0aj7AoGAKfzvVm/AdJ+4/ACrBULM\n6pCaMKefJWxI6u1DpmofObpqozE42HPUyt4gEmUIG2wEIglPpC3UiW8E4Cb5p2hT\nrEgEpX9GDN++vyPKl8TVSpf//gK2DKAhpRRZG074k4SrzbY7Yfa1Z0zt8DuOZ6iQ\neSsIvJ7DzLlMVnjD4l7Pncc=\n-----END PRIVATE KEY-----\n";

    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn config(pairs: &[(&str, &str)]) -> Result<Option<FcmConfig>, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        FcmConfig::from_getter(|key| map.get(key).cloned())
    }

    fn service_account_json() -> String {
        json!({
            "type": "service_account",
            "project_id": "brevity-proj",
            "client_email": "push@brevity-proj.iam.gserviceaccount.com",
            "private_key": TEST_RSA,
        })
        .to_string()
    }

    fn provider() -> FcmProvider {
        let cfg = config(&[("FCM_SERVICE_ACCOUNT_JSON", &service_account_json())])
            .unwrap()
            .expect("configured");
        FcmProvider::new(cfg).expect("valid key")
    }

    #[test]
    fn message_body_matches_legacy_flat_data() {
        let body = build_message("dev-token", "pid", "0xdead");
        let message = &body["message"];
        assert_eq!(message["token"], "dev-token");
        assert_eq!(message["data"]["pushType"], "chat");
        assert_eq!(message["data"]["pushId"], "pid");
        assert_eq!(message["data"]["message"], "0xdead");
        assert_eq!(message["android"]["priority"], "high");
    }

    #[test]
    fn config_is_none_absent_and_validated_when_present() {
        assert!(config(&[]).unwrap().is_none());
        assert!(matches!(
            config(&[("FCM_SERVICE_ACCOUNT_JSON", "not json")]).unwrap_err(),
            ConfigError::Invalid { .. }
        ));
        assert!(matches!(
            config(&[("FCM_SERVICE_ACCOUNT_JSON", r#"{"project_id":"p"}"#)]).unwrap_err(),
            ConfigError::Invalid { .. }
        ));
        let full = config(&[("FCM_SERVICE_ACCOUNT_JSON", &service_account_json())])
            .unwrap()
            .expect("configured");
        assert_eq!(full.project_id, "brevity-proj");
        assert_eq!(
            full.client_email,
            "push@brevity-proj.iam.gserviceaccount.com"
        );
    }

    #[test]
    fn assertion_is_rs256_with_service_account_issuer_and_scope() {
        let token = provider().assertion(1_700_000_000).expect("assertion");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");

        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "push@brevity-proj.iam.gserviceaccount.com");
        assert_eq!(claims["scope"], FCM_SCOPE);
        assert_eq!(claims["aud"], TOKEN_ENDPOINT);
        assert_eq!(claims["exp"], 1_700_003_600i64);
    }

    #[test]
    fn success_reports_sent_and_message_id_without_failed() {
        let outcome = fcm_success(Some("projects/p/messages/123".to_string()));
        assert!(outcome.success);
        assert_eq!(outcome.sent, Some(1));
        assert_eq!(outcome.failed, None);
        assert_eq!(
            outcome.message_id.as_deref(),
            Some("projects/p/messages/123")
        );
    }

    #[test]
    fn error_message_is_concise_with_fallbacks() {
        let body = r#"{"error":{"code":404,"status":"NOT_FOUND","message":"Requested entity was not found.","details":[{"errorCode":"UNREGISTERED"}]}}"#;
        assert_eq!(
            fcm_error_message(body, 404),
            "Requested entity was not found."
        );
        assert_eq!(fcm_error_code(body).as_deref(), Some("UNREGISTERED"));
        // Falls back to the status when there is no parseable error body.
        assert_eq!(
            fcm_error_message("not json", 503),
            "FCM rejected the push (status 503)"
        );
    }
}
