//! **Temporary** Google-API fallback: decode classic integrity tokens via
//! `playintegrity.googleapis.com/v1/{package}:decodeIntegrityToken` with the
//! legacy `GOOGLE_CREDENTIALS` service account.
//!
//! Used only while the self-managed keys are unset (they take precedence),
//! since it reintroduces the Google runtime dependency and the shared
//! 10k/day quota. Delete this module once the key exchange lands.

use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use super::token::TokenPayload;

const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const PLAY_INTEGRITY_SCOPE: &str = "https://www.googleapis.com/auth/playintegrity";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Refresh the access token this many seconds before it actually expires.
const TOKEN_REFRESH_BUFFER_SECS: i64 = 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Validated service-account credentials (from `GOOGLE_CREDENTIALS`).
#[derive(Clone)]
pub struct GoogleCredentials {
    /// Service-account client email (the assertion issuer).
    pub client_email: String,
    /// Service-account RSA private key (PEM).
    pub private_key: String,
}

/// Hand-written so the service-account private key never reaches logs.
impl std::fmt::Debug for GoogleCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCredentials")
            .field("client_email", &self.client_email)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl GoogleCredentials {
    /// Parse the legacy `GOOGLE_CREDENTIALS` value: a base64-encoded
    /// service-account JSON (raw JSON accepted too, for local dev).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let json_bytes = base64::engine::general_purpose::STANDARD
            .decode(trimmed)
            .unwrap_or_else(|_| trimmed.as_bytes().to_vec());

        #[derive(Deserialize)]
        struct ServiceAccount {
            #[serde(default)]
            client_email: String,
            #[serde(default)]
            private_key: String,
        }
        let account: ServiceAccount = serde_json::from_slice(&json_bytes)
            .map_err(|e| format!("not valid service-account JSON: {e}"))?;
        if account.client_email.is_empty() || account.private_key.is_empty() {
            return Err("missing client_email or private_key".to_string());
        }
        // Fail fast on an unusable key rather than at first login.
        EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .map_err(|e| format!("private_key is not a usable RSA PEM: {e}"))?;
        Ok(Self {
            client_email: account.client_email,
            private_key: account.private_key,
        })
    }
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

/// OAuth2-authenticated `decodeIntegrityToken` client with a cached token.
pub struct GoogleDecoder {
    client: reqwest::Client,
    credentials: GoogleCredentials,
    encoding_key: EncodingKey,
    token: Mutex<Option<CachedToken>>,
}

impl GoogleDecoder {
    /// Build a decoder from validated credentials.
    pub fn new(credentials: GoogleCredentials) -> Result<Self, String> {
        let encoding_key = EncodingKey::from_rsa_pem(credentials.private_key.as_bytes())
            .map_err(|e| format!("google credentials private key: {e}"))?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            client,
            credentials,
            encoding_key,
            token: Mutex::new(None),
        })
    }

    /// Decode an integrity token via Google, returning the verdict payload.
    pub async fn decode(
        &self,
        package_name: &str,
        integrity_token: &str,
    ) -> Result<TokenPayload, String> {
        let access_token = self.access_token().await?;
        let url =
            format!("https://playintegrity.googleapis.com/v1/{package_name}:decodeIntegrityToken");
        let response = self
            .client
            .post(&url)
            .bearer_auth(&access_token)
            .json(&serde_json::json!({ "integrityToken": integrity_token }))
            .send()
            .await
            .map_err(|e| format!("decodeIntegrityToken request failed: {e}"))?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("decodeIntegrityToken body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "decodeIntegrityToken rejected ({}): {}",
                status.as_u16(),
                google_error_message(&body)
            ));
        }
        parse_decode_response(&body)
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
    fn assertion(&self, now: i64) -> Result<String, String> {
        let claims = AssertionClaims {
            iss: &self.credentials.client_email,
            scope: PLAY_INTEGRITY_SCOPE,
            aud: TOKEN_ENDPOINT,
            iat: now,
            exp: now + 3600,
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.encoding_key)
            .map_err(|e| format!("assertion: {e}"))
    }

    /// Return a cached access token, exchanging a fresh assertion when stale.
    async fn access_token(&self) -> Result<String, String> {
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
            .map_err(|e| format!("token exchange failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "token exchange rejected ({}): {body}",
                status.as_u16()
            ));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| format!("token response: {e}"))?;
        let expires_at = now + token.expires_in - TOKEN_REFRESH_BUFFER_SECS;
        *self.token.lock().expect("token lock") = Some(CachedToken {
            token: token.access_token.clone(),
            expires_at,
        });
        Ok(token.access_token)
    }
}

/// Parse a `decodeIntegrityToken` response body into the verdict payload.
fn parse_decode_response(body: &[u8]) -> Result<TokenPayload, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DecodeResponse {
        token_payload_external: Option<TokenPayload>,
    }
    let response: DecodeResponse =
        serde_json::from_slice(body).map_err(|e| format!("decodeIntegrityToken response: {e}"))?;
    response
        .token_payload_external
        .ok_or_else(|| "decodeIntegrityToken response has no tokenPayloadExternal".to_string())
}

/// Extract a concise `error.message`/`error.status` for logs.
fn google_error_message(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            let error = value.get("error")?;
            error
                .get("message")
                .or_else(|| error.get("status"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unparseable error body".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Throwaway 2048-bit RSA PKCS#8 key for tests only — NOT a real key
    // (same fixture as the notifications FCM tests).
    const TEST_RSA: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDIx8ZRNzzE1j7T\nmKphU2VTJj6tsJkkJepRMNrSOFS7TrwmGG3LmgoqMpaKk6mgaASjtgayQy7JkXWS\n+rLjoriA3BiMN7HQR+LbMdz1xTQ4IqROZg9V63iz6LiRaOK1Iy6O72syZu9WFSeC\nKqpbOhVPLOukc80mvYMNv52nWjlE8//9Xr3MDnwuoIS7zGXCpqeRkWTFzlTJT6UJ\nboGOWQmdzVnb420kW5Eh+tAl2VG10NiS53BRtvtKb60FPs6XaE4oYOeO7DquNeDU\n9suyFZTtO2TDZgBlu2wdewTX4IQlK6laWHh0OxI0kyoRr4XkcCciu2Mnm++chFfF\nO+/Do/urAgMBAAECggEAXxH5XcuCAtJPvXG0e5E5kT2ywBu2597sLLWtZ4000dxe\nSsuWcanm1kRN9CAnWcTnLX2rfg1BwkAWJBqBSjhqlNmi5gIZOD5RtOjPb7CJIWuF\n0XmSGWT+KnIvsuqvLoYSLaxAXgVPnrs1kfUR5VCDdpIWojATHBzwSTpKNEwMghEQ\nR/L0E6s5+PGEhW51ROJjkI581uL83khJQIGXQwAimcH0nVrQ9aq7yrUDM83ptSx2\nO34yMA4OKyGwJz931HMmVuKj0BL/3il60kSQpLX5AHncUHNrAXfegSuMI9vYBW6q\nh4W9Gv6ctL60sTtmOiN5MNuKLqKLLyNTKpE1U+HEQQKBgQDvZojpf7A6aIxq4okF\nnYYQYgWvRUOxbmYCIvdTQ6dAHpsvcXHwRL9g88evuoxwrzWE76FKEaiFPjhDYXPa\nAgg3PIcNRFcnMHjJZw84SM3Gwb7JWrWkS2+RKndZHHb/4C9ZES9LFCHDDjQkd8iD\nf46Y1wBoYIGDbzj9rYyRvRMwRQKBgQDWs7cKsNIIt+0vG7Js2tF1CpIh0ugO2k8k\ngri3l6a4MLBMV/gRfk0L51AdxcWsROwk6afX3GZiCoBxjTmjktfIsuNVe/UZ8wDL\ns1Z94sqQJWQ3AmWOroAf6CyLdvlbV1fqu3biAF+ZCcQ/nM4y/NtTIJiBIOKj/voo\nSFqx3XUTLwKBgQDNd/taLLVb7A9YTUW9BA3kUbz/STtoNZBnlQsg85fAeIRIm91m\nkhqPY6unLz0KGdadWe3cXHt+oIA5lJKSMdxLTC+9O0Jx6DBC66ksbY/vXqoYtzne\n4L/In+H/IWchBZCdqRomHgk8GBy4j/YQppIEq8M10l5WKEeJskJLczPc4QKBgECd\nByoNergq+hNiR3khBUYu7zmEqlfF9WlsecCuv/rQlE31b298Th2V+HNtUIb+mv6k\n3uFEr/8zX+JMeRs0FwKMa4QPcPzlN8kV6KKr/QAScK1paDzfYSm5CqbSIEsP5yJ6\nVlW/fQWmfcwTGa8Yj7zdo2fBCNQH6Sr0U28R0aj7AoGAKfzvVm/AdJ+4/ACrBULM\n6pCaMKefJWxI6u1DpmofObpqozE42HPUyt4gEmUIG2wEIglPpC3UiW8E4Cb5p2hT\nrEgEpX9GDN++vyPKl8TVSpf//gK2DKAhpRRZG074k4SrzbY7Yfa1Z0zt8DuOZ6iQ\neSsIvJ7DzLlMVnjD4l7Pncc=\n-----END PRIVATE KEY-----\n";

    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn service_account_json() -> String {
        json!({
            "type": "service_account",
            "project_id": "polkadot-app",
            "client_email": "integrity@polkadot-app.iam.gserviceaccount.com",
            "private_key": TEST_RSA,
        })
        .to_string()
    }

    #[test]
    fn credentials_parse_base64_and_raw_json_and_reject_garbage() {
        let raw = service_account_json();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);

        for input in [raw.as_str(), encoded.as_str()] {
            let creds = GoogleCredentials::parse(input).expect("parses");
            assert_eq!(
                creds.client_email,
                "integrity@polkadot-app.iam.gserviceaccount.com"
            );
        }

        assert!(GoogleCredentials::parse("not json").is_err());
        assert!(GoogleCredentials::parse(r#"{"client_email":"a@b.c"}"#).is_err());
        assert!(
            GoogleCredentials::parse(r#"{"client_email":"a@b.c","private_key":"not a pem"}"#)
                .is_err()
        );
    }

    #[test]
    fn assertion_is_rs256_with_the_playintegrity_scope() {
        let creds = GoogleCredentials::parse(&service_account_json()).unwrap();
        let decoder = GoogleDecoder::new(creds).unwrap();
        let assertion = decoder.assertion(1_700_000_000).expect("assertion");

        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims["iss"],
            "integrity@polkadot-app.iam.gserviceaccount.com"
        );
        assert_eq!(claims["scope"], PLAY_INTEGRITY_SCOPE);
        assert_eq!(claims["aud"], TOKEN_ENDPOINT);
        assert_eq!(claims["exp"], 1_700_003_600i64);
    }

    #[test]
    fn decode_response_parses_the_verdict_payload_shape() {
        // Shape of a real decodeIntegrityToken response (Google API docs /
        // legacy `Schema$DecodeIntegrityTokenResponse`).
        let body = json!({
            "tokenPayloadExternal": {
                "requestDetails": {
                    "requestPackageName": "io.pcf.polkadotapp",
                    "nonce": "bm9uY2U",
                    "timestampMillis": "1750000000000"
                },
                "appIntegrity": {
                    "appRecognitionVerdict": "PLAY_RECOGNIZED",
                    "packageName": "io.pcf.polkadotapp",
                    "certificateSha256Digest": ["digest"],
                    "versionCode": "42"
                },
                "deviceIntegrity": {
                    "deviceRecognitionVerdict": ["MEETS_DEVICE_INTEGRITY"]
                },
                "accountDetails": { "appLicensingVerdict": "LICENSED" }
            }
        })
        .to_string();

        let payload = parse_decode_response(body.as_bytes()).expect("parses");
        assert_eq!(
            payload.request_details.unwrap().nonce.as_deref(),
            Some("bm9uY2U")
        );
        assert_eq!(
            payload
                .app_integrity
                .unwrap()
                .app_recognition_verdict
                .as_deref(),
            Some("PLAY_RECOGNIZED")
        );

        assert!(parse_decode_response(b"{}").is_err());
        assert!(parse_decode_response(b"not json").is_err());
    }

    #[test]
    fn error_message_extraction_is_concise() {
        let body = br#"{"error":{"code":400,"message":"Integrity token cannot be decoded.","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(
            google_error_message(body),
            "Integrity token cannot be decoded."
        );
        assert_eq!(google_error_message(b"nope"), "unparseable error body");
    }
}
