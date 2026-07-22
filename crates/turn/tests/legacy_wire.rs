//! Golden legacy-fixture replay: pins the Rust wire contract against
//! request/response pairs captured from the REAL legacy TypeScript route
//! (`fixtures/legacy-wire.json`; provenance in `fixtures/README.md`).
//!
//! Every fixture replays through the full production router offline (the
//! service is stateless — there is nothing to stub):
//! - **Deterministic cases** (the 401 trio, the 400 matrix, malformed/empty
//!   JSON, the 404 fallbacks) compare status/content-type/`Retry-After`/body
//!   exactly.
//! - **201 cases** carry capture-time credentials (expiry = capture now + TTL,
//!   id = 8 random bytes), so byte equality is impossible. Instead each one
//!   pins the contract cryptographically: the fixture's own username/password
//!   pair must verify under the pinned capture secret (proving the legacy
//!   construction), and the live response's password must equal the HMAC over
//!   its username under the same secret (proving this service mints the same
//!   construction), plus exact `servers`/`ttl` and username-shape assertions.
//!
//! The 429 rate-limit dialect is pinned here without a captured fixture (the
//! capture did not mount the legacy limiter; its body is transcribed from the
//! legacy middleware source, as with the other services).
//!
//! JWTs: the legacy capture authenticated with HS256 (the legacy secret);
//! this service verifies identity-service Ed25519 tokens, so authenticated
//! replays mint an EdDSA token for a test key.

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use tower::ServiceExt as _;
use turn::credentials::{Algorithm, Issuer};
use turn::{AppState, Config};

const FIXTURES: &str = include_str!("fixtures/legacy-wire.json");

const JWT_SEED: [u8; 32] = [42u8; 32];
const SUBJECT: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

/// The capture's pinned issuer config (see `fixtures/capture/…`): 32 bytes of
/// 0x42, TTL 1800s, the two example ICE servers.
const CAPTURE_SECRET: [u8; 32] = [0x42; 32];
const TTL_SECS: u64 = 1800;
const ICE_SERVERS: [&str; 2] = [
    "stun:stun.example.com:3478",
    "turn:turn.example.com:3478?transport=udp",
];

#[derive(Deserialize)]
struct Fixture {
    name: String,
    request: FixtureRequest,
    response: FixtureResponse,
}

#[derive(Deserialize)]
struct FixtureRequest {
    method: String,
    path: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct FixtureResponse {
    status: u16,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    #[serde(rename = "retryAfter")]
    retry_after: Option<String>,
    body: serde_json::Value,
}

fn fixtures() -> Vec<Fixture> {
    serde_json::from_str(FIXTURES).expect("fixture file parses")
}

/// Mint an EdDSA access token for the test verifier key.
fn mint_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs() as i64
        + 3600;
    let key = SigningKey::from_bytes(&JWT_SEED);
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"accountId":"{SUBJECT}","sub":"{SUBJECT}","exp":{exp}}}"#
    ));
    let payload = format!("{header}.{claims}");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(payload.as_bytes()).to_bytes());
    format!("{payload}.{signature}")
}

/// The production router over the capture's pinned config.
fn app(rate_limit: u32, algorithm: Algorithm) -> axum::Router {
    let key = SigningKey::from_bytes(&JWT_SEED);
    let config = Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        turn_secret: CAPTURE_SECRET.to_vec(),
        algorithm,
        ttl_secs: TTL_SECS,
        realm: "example.org".to_string(),
        ice_servers: ICE_SERVERS.iter().map(|s| s.to_string()).collect(),
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        rate_limit,
        rate_window: Duration::from_secs(60),
    };
    turn::routes(AppState::new(config))
}

/// The capture-secret issuer, used to verify passwords on both sides.
fn capture_issuer(algorithm: Algorithm) -> Issuer {
    Issuer::new(CAPTURE_SECRET.to_vec(), algorithm, TTL_SECS)
}

fn build_request(fixture: &Fixture, token: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(fixture.request.method.as_str())
        .uri(&fixture.request.path);
    for (name, value) in &fixture.request.headers {
        // The capture authenticated with a legacy HS256 token; replay with a
        // minted EdDSA one. Deliberately-broken auth headers replay verbatim.
        if name.eq_ignore_ascii_case("authorization") && value.starts_with("Bearer eyJ") {
            builder = builder.header(name, format!("Bearer {token}"));
        } else {
            builder = builder.header(name, value);
        }
    }
    let body = fixture.request.body.clone().unwrap_or_default();
    builder.body(Body::from(body)).expect("request")
}

struct Observed {
    status: u16,
    content_type: Option<String>,
    retry_after: Option<String>,
    body_text: String,
}

impl Observed {
    async fn from(response: axum::response::Response) -> Self {
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let retry_after = response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        Self {
            status,
            content_type,
            retry_after,
            body_text: String::from_utf8_lossy(&body).to_string(),
        }
    }

    /// Compare against a captured fixture response. JSON bodies compare as
    /// values (key order is not part of the contract); text bodies as bytes.
    fn assert_matches(&self, name: &str, want: &FixtureResponse) {
        assert_eq!(self.status, want.status, "{name}: status");
        assert_eq!(
            self.content_type.as_deref(),
            want.content_type.as_deref(),
            "{name}: content-type"
        );
        assert_eq!(
            self.retry_after.as_deref(),
            want.retry_after.as_deref(),
            "{name}: retry-after"
        );
        match &want.body {
            serde_json::Value::String(text) => {
                assert_eq!(&self.body_text, text, "{name}: text body");
            }
            expected => {
                let got: serde_json::Value = serde_json::from_str(&self.body_text)
                    .unwrap_or_else(|_| panic!("{name}: body is not JSON: {}", self.body_text));
                assert_eq!(&got, expected, "{name}: JSON body");
            }
        }
    }
}

/// Assert a credential body's frozen shape and cryptographic construction.
fn assert_credentials(name: &str, body: &serde_json::Value, issuer: &Issuer, live: bool) {
    assert_eq!(
        body["servers"],
        serde_json::json!(ICE_SERVERS),
        "{name}: servers"
    );
    assert_eq!(body["ttl"], serde_json::json!(TTL_SECS), "{name}: ttl");

    let username = body["username"].as_str().expect("username string");
    let (expiry, id) = username.split_once(':').expect("expiry:id form");
    let expiry: u64 = expiry
        .parse()
        .unwrap_or_else(|_| panic!("{name}: numeric expiry"));
    assert_eq!(id.len(), 16, "{name}: id is 8 hex-encoded bytes");
    assert!(hex::decode(id).is_ok(), "{name}: id is valid hex");
    if live {
        // A freshly minted credential expires ~TTL from now.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs();
        assert!(
            expiry >= now + TTL_SECS - 5 && expiry <= now + TTL_SECS + 5,
            "{name}: expiry {expiry} not ~now+{TTL_SECS}"
        );
    }

    // The construction itself: password = base64(HMAC(secret, username)).
    let password = body["password"].as_str().expect("password string");
    assert_eq!(
        password,
        issuer.password_for(username),
        "{name}: password is not the HMAC over the username"
    );
}

/// 201 fixture names, with the algorithm their capture app used.
fn dynamic_algorithm(name: &str) -> Option<Algorithm> {
    match name {
        "post-201-sha256" => Some(Algorithm::Sha256),
        "post-missing-content-type"
        | "post-no-body-at-all"
        | "post-201-empty-object"
        | "post-201-region-hint"
        | "post-201-null-region" => Some(Algorithm::Sha1),
        _ => None,
    }
}

#[tokio::test]
async fn deterministic_fixtures_replay_through_the_production_router() {
    let app = app(1000, Algorithm::Sha1);
    let token = mint_token();
    let mut replayed = 0;

    for fixture in fixtures() {
        if dynamic_algorithm(&fixture.name).is_some() {
            continue;
        }
        let response = app
            .clone()
            .oneshot(build_request(&fixture, &token))
            .await
            .expect("response");
        Observed::from(response)
            .await
            .assert_matches(&fixture.name, &fixture.response);
        replayed += 1;
    }
    // The matrix is frozen: a shrinking fixture file must fail loudly.
    assert_eq!(replayed, 12, "deterministic fixture count changed");
}

#[tokio::test]
async fn credential_fixtures_verify_cryptographically_on_both_sides() {
    let token = mint_token();
    let mut replayed = 0;

    for fixture in fixtures() {
        let Some(algorithm) = dynamic_algorithm(&fixture.name) else {
            continue;
        };
        let issuer = capture_issuer(algorithm);

        // The captured legacy credentials verify under the pinned secret —
        // the legacy issuer used exactly this construction.
        assert_eq!(fixture.response.status, 201, "{}", fixture.name);
        assert_credentials(&fixture.name, &fixture.response.body, &issuer, false);

        // The live replay mints credentials with the same construction.
        let app = app(1000, algorithm);
        let response = app
            .oneshot(build_request(&fixture, &token))
            .await
            .expect("response");
        let observed = Observed::from(response).await;
        assert_eq!(observed.status, 201, "{}: live status", fixture.name);
        assert_eq!(
            observed.content_type.as_deref(),
            Some("application/json"),
            "{}: live content-type",
            fixture.name
        );
        let body: serde_json::Value =
            serde_json::from_str(&observed.body_text).expect("live JSON body");
        assert_credentials(&fixture.name, &body, &issuer, true);
        replayed += 1;
    }
    assert_eq!(replayed, 6, "credential fixture count changed");
}

// --- pinned without a capture (see fixtures/README.md) ---------------------

#[tokio::test]
async fn rate_limit_returns_the_transcribed_429_after_the_window_fills() {
    // Limit 2: two mints succeed, the third is refused with Retry-After.
    let app = app(2, Algorithm::Sha1);
    let token = mint_token();
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/turn/issue")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("request")
    };

    for _ in 0..2 {
        let observed =
            Observed::from(app.clone().oneshot(request()).await.expect("response")).await;
        assert_eq!(observed.status, 201);
    }

    let observed = Observed::from(app.clone().oneshot(request()).await.expect("response")).await;
    assert_eq!(observed.status, 429);
    assert_eq!(observed.retry_after.as_deref(), Some("60"));
    assert_eq!(
        observed.content_type.as_deref(),
        Some("application/problem+json")
    );
    let got: serde_json::Value = serde_json::from_str(&observed.body_text).expect("json");
    assert_eq!(
        got,
        serde_json::json!({
            "type": "https://problems-registry.smartbear.com/too-many-requests",
            "title": "Too Many Requests",
            "detail": "Rate limit exceeded. Please retry after 60 seconds.",
            "status": 429,
        })
    );

    // Invalid bodies 400 before the limiter — they never consume the window.
    let bad = Request::builder()
        .method("POST")
        .uri("/api/v1/turn/issue")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from("[1]"))
        .expect("request");
    let observed = Observed::from(app.oneshot(bad).await.expect("response")).await;
    assert_eq!(observed.status, 400);
}
