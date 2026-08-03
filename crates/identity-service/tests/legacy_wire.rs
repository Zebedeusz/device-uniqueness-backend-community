//! Golden legacy-fixture replay: pins the Rust wire contract against
//! request/response pairs captured from the REAL legacy TypeScript usernames
//! router (`fixtures/legacy-wire.json`; provenance in `fixtures/README.md`).
//!
//! The 55 captured POST-surface fixtures split into eight frozen groups.
//! Since 2026-07-23 the error *bodies* are no longer byte-frozen (no client
//! parses them; the 401/404/409 status codes are kept) and the captured
//! intentional 500s are normal 400s — the affected captures moved into the
//! divergence groups below.
//!
//! - **Offline router replay** — everything whose path reaches neither the
//!   database nor the chain (the register auth trio, the 400 validation
//!   matrix, the CORS preflight, the 404 fallbacks) drives the full
//!   production router over a dead lazy pool + offline chain client.
//! - **dotns-disabled replay** — the captured disabled-gateway 400 replays
//!   through a second router built with `dotns_gateway_enabled = false`.
//! - **Hardening divergence** — legacy served `/available` unauthenticated
//!   (the three probes captured 200). This service keeps the JWT gate: the
//!   probes replay asserting the frozen 401 trio instead.
//! - **Retired-dialect divergence** — the Hono text malformed-JSON 400 is a
//!   normal JSON 400 now, and the regex-failed `candidateSignature` opaque
//!   500 quirk is a normal 400 field error; the four captures replay offline
//!   asserting the DIVERGENT responses. The two missing-content-type quirk
//!   captures (available's opaque 500, register's silent empty-object
//!   substitution) are evidence only: bodies now parse regardless of content
//!   type, so those requests proceed to chain/DB-dependent success paths.
//! - **v0-removal divergence** — the availability v0 response shape was
//!   removed 2026-07-23 (unused; both shipping apps send `?version=v1`).
//!   Two captures replay offline asserting the DIVERGENT response (v1 is
//!   the default, `v0` rejects); the chain-dependent v0-shape 200s are
//!   retained as evidence of the retired shape, never replayed for parity.
//! - **Response replay** — chain/DB-dependent cases compare the exact bodies
//!   the handlers emit (the 202s, the 409 renderings, the v1 availability
//!   200 reconstructed from the capture's seed truth, the dotns freshness
//!   400s whose `signedAt` values were TestClock-relative).
//! - **Deferred DeviceCheck** — the attestation-slice lanes, parked frozen.
//! - The 429 and the insert-race/persistence bodies are pinned without
//!   captures (transcribed; see `fixtures/README.md`).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use identity_service::usernames::error::UsernamesError;
use identity_service::usernames::register::RegisterResponse;
use identity_service::{AppState, ChainClient, Config, Jwt};
use serde::Deserialize;
use tower::ServiceExt as _;

const FIXTURES: &str = include_str!("fixtures/legacy-wire.json");

const JWT_SEED: [u8; 32] = [42u8; 32];
const SUBJECT: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

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
    headers: std::collections::BTreeMap<String, String>,
    body: serde_json::Value,
}

fn fixtures() -> Vec<Fixture> {
    serde_json::from_str(FIXTURES).expect("fixture file parses")
}

fn find_fixture(name: &str) -> Fixture {
    fixtures()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fixture {name} exists"))
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

/// An offline People Chain client: real `OnlineClient` machinery over a mock
/// RPC that answers only the genesis-hash handshake. Any actual read fails —
/// exactly what the offline replay wants.
async fn dead_chain_client() -> ChainClient {
    use subxt_rpcs::client::mock_rpc_client::Json;
    use subxt_rpcs::client::{MockRpcClient, RpcClient};

    let mock = MockRpcClient::builder()
        .method_handler("chain_getBlockHash", |_params| async {
            Json(serde_json::json!(format!("0x{}", "00".repeat(32))))
        })
        .build();
    let backend = subxt::backend::LegacyBackend::builder().build(RpcClient::new(mock));
    let client = subxt::OnlineClient::<chain_types::PeopleConfig>::from_backend(Arc::new(backend))
        .await
        .expect("offline client from mock backend");
    ChainClient::from_online(client)
}

/// The production router over a lazy pool that connects to nothing: no
/// replayed offline fixture may complete a DB or chain read.
async fn offline_app(dotns_enabled: bool, rate_limit: u32) -> axum::Router {
    offline_app_with_queue(dotns_enabled, rate_limit, false).await
}

async fn offline_app_with_queue(
    dotns_enabled: bool,
    rate_limit: u32,
    queue_enabled: bool,
) -> axum::Router {
    let mut config = Config::test_default();
    config.dotns_gateway_enabled = dotns_enabled;
    config.auth_rate_limit = rate_limit;
    config.queue_enabled = queue_enabled;
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://nobody@127.0.0.1:1/nothing")
        .expect("lazy pool");
    identity_service::routes(AppState::new(pool, dead_chain_client().await, jwt, config))
}

struct Observed {
    status: u16,
    content_type: Option<String>,
    retry_after: Option<String>,
    headers: axum::http::HeaderMap,
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
        let headers = response.headers().clone();
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
            headers,
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
        if self.status == 204 {
            return; // no body on the preflight
        }
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

async fn replay(app: &axum::Router, fixture: &Fixture, token: &str) -> Observed {
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
    let request = builder.body(Body::from(body)).expect("request");
    Observed::from(app.clone().oneshot(request).await.expect("response")).await
}

#[tokio::test]
async fn payment_status_route_is_mounted_only_when_payment_lane_enabled() {
    let request = || {
        Request::builder()
            .method("GET")
            .uri("/api/v1/usernames/payment-status")
            .body(Body::empty())
            .expect("request")
    };

    // Payment lane off (the default test config): the global Hono 404.
    let disabled = offline_app(true, 100_000).await;
    let observed = Observed::from(
        disabled
            .oneshot(request())
            .await
            .expect("disabled response"),
    )
    .await;
    assert_eq!(observed.status, 404);
    assert_eq!(observed.body_text, "404 Not Found");

    // Payment lane on: the route exists and reaches its JWT extractor.
    let mut config = Config::test_default();
    config.payment = Some(identity_service::config::PaymentConfig {
        master_account: [7u8; 32],
        amount_planck: 10_000_000_000,
        request_ttl: Duration::from_secs(3600),
    });
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://nobody@127.0.0.1:1/nothing")
        .expect("lazy pool");
    let enabled =
        identity_service::routes(AppState::new(pool, dead_chain_client().await, jwt, config));
    let observed =
        Observed::from(enabled.oneshot(request()).await.expect("enabled response")).await;
    assert_eq!(
        observed.status, 401,
        "enabled route reaches its JWT extractor"
    );
}

#[tokio::test]
async fn registration_queue_route_is_mounted_only_when_enabled() {
    let request = || {
        Request::builder()
            .method("GET")
            .uri("/api/v1/registration/queue")
            .body(Body::empty())
            .expect("request")
    };

    let disabled = offline_app_with_queue(true, 100_000, false).await;
    let observed = Observed::from(
        disabled
            .oneshot(request())
            .await
            .expect("disabled response"),
    )
    .await;
    assert_eq!(observed.status, 404);
    assert_eq!(observed.body_text, "404 Not Found");

    let enabled = offline_app_with_queue(true, 100_000, true).await;
    let observed =
        Observed::from(enabled.oneshot(request()).await.expect("enabled response")).await;
    assert_eq!(
        observed.status, 401,
        "enabled route reaches its JWT extractor"
    );
}

/// Fixtures replayed through the full production router offline, expecting a
/// byte-frozen match.
const OFFLINE_ROUTER: &[&str] = &[
    "available-400-usernames-missing",
    "available-400-usernames-null",
    "available-400-usernames-not-array",
    "available-400-usernames-101-items",
    "available-400-non-string-element",
    "available-cors-preflight",
    "register-auth-probe-missing",
    "register-auth-probe-basic",
    "register-auth-probe-bad-bearer",
    "register-400-invalid-signature",
    "register-400-invalid-ss58",
    "register-400-username-too-long",
    "register-400-empty-object",
    "register-400-wrong-types",
    "register-400-username-short",
    "register-400-username-uppercase",
    "register-400-preferred-digits-00",
    "register-400-preferred-digits-single",
    "register-400-preferred-digits-triple",
    "register-400-identifier-wrong-length",
    "register-400-ringvrf-not-hex",
    "register-400-multi-failure",
    "register-400-device-token-bad-base64",
    "register-cors-preflight",
    "post-404-username-path",
    "delete-404-collection",
];

/// Replays through a router with `dotns_gateway_enabled = false` (the
/// capture's Mount A environment).
const OFFLINE_DOTNS_DISABLED: &[&str] = &["register-400-dotns-disabled"];

/// Legacy served `/available` completely unauthenticated (captured 200s).
/// This service keeps the JWT gate as recorded hardening: the probes must
/// yield the frozen 401 trio instead.
const HARDENING_DIVERGENCE: &[&str] = &[
    "available-auth-probe-missing",
    "available-auth-probe-basic",
    "available-auth-probe-bad-bearer",
];

/// Captures of the retired error dialect that replay offline asserting the
/// DIVERGENT response (2026-07-23; statuses stay meaningful, bodies are no
/// longer byte-frozen): the Hono text malformed-JSON 400 is a normal JSON
/// 400, and the regex-failed `candidateSignature` opaque 500 quirk is the
/// plain pattern 400 field error.
const RETIRED_DIALECT_REPLAY: &[&str] = &[
    "available-400-malformed-json",
    "register-400-malformed-json",
    "register-400-signature-not-hex",
    "register-400-signature-wrong-length",
];

/// Captures of the retired missing-content-type quirks (available's opaque
/// 500, register's silent empty-object substitution → all-fields-missing
/// 400), retained as evidence only: bodies now parse as JSON regardless of
/// content type, so both requests proceed to chain/DB-dependent success
/// paths that cannot replay offline.
const CONTENT_TYPE_QUIRK_EVIDENCE: &[&str] = &[
    "available-missing-content-type",
    "register-missing-content-type",
];

/// Availability v0 captures that replay offline asserting the DIVERGENT
/// response: the v0 shape was removed 2026-07-23 (unused — both shipping
/// apps send `?version=v1`), so a missing `?version` now serves v1 and a
/// non-`v1` value (including `v0`) rejects with the updated option list.
const V0_REMOVED_REPLAY: &[&str] = &["available-200-empty-array", "available-400-version-bogus"];

/// Chain-dependent captures of the removed v0 response shape (flat
/// `name → status` record), retained as evidence only — the shape no longer
/// exists to replay against. `available-200-duplicates` was captured without
/// `?version`, so its body is v0-shaped too.
const V0_SHAPE_EVIDENCE: &[&str] = &[
    "available-200-v0-default",
    "available-200-v0-explicit",
    "available-200-duplicates",
];

/// Chain/DB-dependent captures whose renderings replay at response altitude.
const RESPONSE_REPLAY: &[&str] = &[
    "available-200-v1",
    "register-409-preferred-taken",
    "register-409-no-digits",
    "register-202-random-digit",
    "register-202-preferred-digit",
    "register-202-devicecheck-proceed-none",
    "register-202-dotns-valid",
    "register-400-dotns-future",
    "register-400-dotns-stale",
];

/// DeviceCheck lanes deferred to the attestation slice (fixtures parked).
const DEFERRED_DEVICECHECK: &[&str] = &[
    "register-200-devicecheck-payment-required",
    "register-401-devicecheck-token-required",
    "register-202-devicecheck-register-true",
    "register-202-devicecheck-proceed-false",
    "register-500-ios-registration-failed",
];

#[test]
fn every_fixture_is_classified_exactly_once() {
    let all = fixtures();
    let mut classified: Vec<&str> = [
        OFFLINE_ROUTER,
        OFFLINE_DOTNS_DISABLED,
        HARDENING_DIVERGENCE,
        RETIRED_DIALECT_REPLAY,
        CONTENT_TYPE_QUIRK_EVIDENCE,
        V0_REMOVED_REPLAY,
        V0_SHAPE_EVIDENCE,
        RESPONSE_REPLAY,
        DEFERRED_DEVICECHECK,
    ]
    .concat();
    classified.sort_unstable();
    let duplicates = classified.windows(2).any(|pair| pair[0] == pair[1]);
    assert!(!duplicates, "a fixture is classified twice");
    let mut names: Vec<String> = all.iter().map(|f| f.name.clone()).collect();
    names.sort_unstable();
    let mut expected: Vec<String> = classified.iter().map(|s| s.to_string()).collect();
    expected.sort_unstable();
    assert_eq!(names, expected, "fixture set and classification diverged");
    assert_eq!(all.len(), 55, "captured fixture count is frozen");
}

#[tokio::test]
async fn offline_fixtures_replay_through_the_production_router() {
    let app = offline_app(true, 100_000).await;
    let token = mint_token();
    for name in OFFLINE_ROUTER {
        let fixture = find_fixture(name);
        replay(&app, &fixture, &token)
            .await
            .assert_matches(name, &fixture.response);
    }
}

#[tokio::test]
async fn dotns_disabled_fixture_replays_through_a_disabled_router() {
    let app = offline_app(false, 100_000).await;
    let token = mint_token();
    for name in OFFLINE_DOTNS_DISABLED {
        let fixture = find_fixture(name);
        replay(&app, &fixture, &token)
            .await
            .assert_matches(name, &fixture.response);
    }
}

#[tokio::test]
async fn collection_preflight_reproduces_the_captured_cors_headers() {
    let app = offline_app(true, 100_000).await;
    let fixture = find_fixture("register-cors-preflight");
    let observed = replay(&app, &fixture, "unused").await;
    for header in [
        "access-control-allow-origin",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "vary",
    ] {
        assert_eq!(
            observed.headers.get(header).and_then(|v| v.to_str().ok()),
            fixture.response.headers.get(header).map(String::as_str),
            "register-cors-preflight: {header}"
        );
    }
}

#[tokio::test]
async fn available_auth_probes_yield_the_frozen_401_trio_hardening_divergence() {
    let app = offline_app(true, 100_000).await;
    // The frozen trio bodies were captured on the register probes.
    for (probe, trio_source) in [
        (
            "available-auth-probe-missing",
            "register-auth-probe-missing",
        ),
        ("available-auth-probe-basic", "register-auth-probe-basic"),
        (
            "available-auth-probe-bad-bearer",
            "register-auth-probe-bad-bearer",
        ),
    ] {
        let fixture = find_fixture(probe);
        assert_eq!(fixture.response.status, 200, "{probe}: legacy served 200");
        let observed = replay(&app, &fixture, "unused").await;
        let want = find_fixture(trio_source).response;
        observed.assert_matches(probe, &want);
    }
}

#[tokio::test]
async fn retired_dialect_divergence_replays_offline() {
    let app = offline_app(true, 100_000).await;
    let token = mint_token();

    // Malformed JSON: legacy answered the Hono plain-text 400; the retired
    // dialect serves a normal JSON 400 with the same message.
    for name in [
        "available-400-malformed-json",
        "register-400-malformed-json",
    ] {
        let fixture = find_fixture(name);
        let observed = replay(&app, &fixture, &token).await;
        assert_eq!(observed.status, 400, "{name}: status");
        assert_eq!(
            observed.content_type.as_deref(),
            Some("application/json"),
            "{name}: content type"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&observed.body_text).expect("json"),
            serde_json::json!({ "error": "Malformed JSON in request body" }),
            "{name}: body"
        );
    }

    // Regex-failed candidateSignature: legacy 500'd (the raw string reached
    // the wasm verifier and threw); now it is the plain pattern field error.
    for name in [
        "register-400-signature-not-hex",
        "register-400-signature-wrong-length",
    ] {
        let fixture = find_fixture(name);
        assert_eq!(fixture.response.status, 500, "{name}: legacy 500'd here");
        let observed = replay(&app, &fixture, &token).await;
        assert_eq!(observed.status, 400, "{name}: status");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&observed.body_text).expect("json"),
            serde_json::json!({
                "type": "https://problems-registry.smartbear.com/invalid-body-property-value",
                "title": "Invalid Body Property Value",
                "detail": "The request body contains an invalid body property value.",
                "status": 400,
                "errors": [{
                    "detail": "Must be a hexadecimal string of exactly 64 bytes.",
                    "pointer": "#/candidateSignature"
                }]
            }),
            "{name}: body"
        );
    }
}

#[tokio::test]
async fn availability_v0_removal_divergence_replays_offline() {
    let app = offline_app(true, 100_000).await;
    let token = mint_token();

    // Captured without `?version` (legacy defaulted to the flat v0 record);
    // v1 is the only shape now, so the empty request serves the tagged
    // envelope instead of the captured `{}`.
    let fixture = find_fixture("available-200-empty-array");
    let observed = replay(&app, &fixture, &token).await;
    assert_eq!(observed.status, 200, "available-200-empty-array: status");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&observed.body_text).expect("json"),
        serde_json::json!({ "_tag": "v1", "value": {} }),
        "available-200-empty-array: v1 envelope"
    );

    // The captured 400 listed both versions; `v0` left the option list when
    // it was removed.
    let fixture = find_fixture("available-400-version-bogus");
    let observed = replay(&app, &fixture, &token).await;
    assert_eq!(observed.status, 400, "available-400-version-bogus: status");
    assert_eq!(
        observed.content_type.as_deref(),
        Some("application/problem+json"),
        "available-400-version-bogus: content type"
    );
    let mut want = fixture.response.body.clone();
    want["errors"][0]["detail"] = serde_json::json!("Invalid option: expected one of \"v1\"");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&observed.body_text).expect("json"),
        want,
        "available-400-version-bogus: body (option list updated)"
    );
}

/// Rebuild an availability response from the capture's seed truth:
/// `takenxx` had digits 42/43 allocated, `crowded` all 100, every other
/// valid base none.
fn expected_availability(names: &[&str], v1: bool) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    for name in names {
        let valid = name.len() >= 6 && name.bytes().all(|b| b.is_ascii_lowercase());
        let entry = if !valid {
            ("INVALID", None)
        } else if *name == "crowded" {
            ("EXHAUSTED", None)
        } else {
            let taken: &[u8] = if *name == "takenxx" { &[42, 43] } else { &[] };
            let digits: Vec<u8> = (1..=99u8).filter(|d| !taken.contains(d)).collect();
            ("AVAILABLE", Some(digits))
        };
        if v1 {
            let mut object = serde_json::Map::new();
            object.insert("status".into(), entry.0.into());
            if let Some(digits) = entry.1 {
                object.insert("availableDigits".into(), serde_json::json!(digits));
            }
            value.insert((*name).to_string(), serde_json::Value::Object(object));
        } else {
            value.insert((*name).to_string(), entry.0.into());
        }
    }
    if v1 {
        serde_json::json!({ "_tag": "v1", "value": value })
    } else {
        serde_json::Value::Object(value)
    }
}

#[tokio::test]
async fn availability_200_bodies_match_the_capture() {
    // The v0 fixtures document the REMOVED flat shape (evidence only, see
    // `V0_SHAPE_EVIDENCE`); only the v1 body is still a live-parity pin.
    let long = "a".repeat(30);
    let names = [
        "openok", "takenxx", "crowded", "abc", "UPPERX", "alice9", &long,
    ];
    for (fixture_name, v1) in [
        ("available-200-v0-default", false),
        ("available-200-v0-explicit", false),
        ("available-200-v1", true),
    ] {
        let fixture = find_fixture(fixture_name);
        assert_eq!(
            expected_availability(&names, v1),
            fixture.response.body,
            "{fixture_name}"
        );
    }
    let fixture = find_fixture("available-200-duplicates");
    assert_eq!(
        expected_availability(&["openok"], false),
        fixture.response.body,
        "available-200-duplicates"
    );
}

#[tokio::test]
async fn register_202_bodies_match_the_capture() {
    for (name, base, digits) in [
        ("register-202-random-digit", "signer", "55"),
        ("register-202-preferred-digit", "prefree", "77"),
        ("register-202-devicecheck-proceed-none", "silentt", "65"),
        ("register-202-dotns-valid", "dotnsok", "67"),
    ] {
        let fixture = find_fixture(name);
        assert_eq!(fixture.response.status, 202, "{name}");
        // The queue fields are `None` here because the capture environment ran
        // queue-disabled — the frozen bodies must stay byte-identical.
        let body = RegisterResponse {
            base_username: base.to_string(),
            digits: digits.to_string(),
            username: format!("{base}.{digits}"),
            device_check_available: None,
            registration_outcome: None,
            queue: None,
        };
        assert_eq!(
            serde_json::to_value(body).expect("serialize"),
            fixture.response.body,
            "{name}"
        );
    }
}

#[tokio::test]
async fn handler_error_renderings_match_the_capture() {
    let cases: Vec<(&str, UsernamesError)> = vec![
        (
            "register-409-preferred-taken",
            UsernamesError::PreferredDigitsTaken {
                digits: "88".to_string(),
                base: "preftkn".to_string(),
            },
        ),
        (
            "register-409-no-digits",
            UsernamesError::NoDigitsAvailable {
                base: "crammed".to_string(),
            },
        ),
        (
            "register-400-dotns-future",
            UsernamesError::InvalidBody(vec![http_common::FieldError {
                detail: "signedAt is in the future (tolerance 600s).".to_string(),
                pointer: "#/dotns/signedAt".to_string(),
            }]),
        ),
        (
            "register-400-dotns-stale",
            UsernamesError::InvalidBody(vec![http_common::FieldError {
                detail: "signedAt is older than the intake freshness bound (600s). \
                         Re-sign with a fresh timestamp and resubmit."
                    .to_string(),
                pointer: "#/dotns/signedAt".to_string(),
            }]),
        ),
    ];
    for (name, error) in cases {
        let fixture = find_fixture(name);
        let observed = Observed::from(error.into_response()).await;
        observed.assert_matches(name, &fixture.response);
    }
}

#[test]
fn deferred_devicecheck_fixtures_stay_frozen() {
    // The attestation slice implements these; until then the captured lanes
    // are parked. Freezing status+name here makes silent fixture drift loud.
    for name in DEFERRED_DEVICECHECK {
        let fixture = find_fixture(name);
        let expected = match *name {
            "register-200-devicecheck-payment-required" => 200,
            "register-401-devicecheck-token-required" => 401,
            "register-202-devicecheck-register-true" => 202,
            "register-202-devicecheck-proceed-false" => 202,
            "register-500-ios-registration-failed" => 500,
            other => panic!("unclassified deferred fixture {other}"),
        };
        assert_eq!(fixture.response.status, expected, "{name}");
    }
}

// --- pinned without a capture (see fixtures/README.md) ---------------------

#[tokio::test]
async fn insert_race_and_persistence_renderings_match_the_transcription() {
    let taken = Observed::from(
        UsernamesError::UsernameTaken {
            base: "aliceuser".to_string(),
            digits: "42".to_string(),
        }
        .into_response(),
    )
    .await;
    assert_eq!(taken.status, 409);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&taken.body_text).expect("json"),
        serde_json::json!({
            "error": "Username aliceuser.42 already taken. Please try different digits."
        })
    );

    let persistence = Observed::from(UsernamesError::PersistenceFailed.into_response()).await;
    assert_eq!(persistence.status, 500);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&persistence.body_text).expect("json"),
        serde_json::json!({ "error": "Failed to persist username registration" })
    );
}

#[tokio::test]
async fn rate_limit_returns_the_transcribed_429_after_the_window_fills() {
    // Limit 2: two register attempts pass the limiter and die at the
    // unreachable chain (the opaque 500), the third is refused.
    let app = offline_app(true, 2).await;
    let token = mint_token();
    let valid = find_fixture("register-202-random-digit");
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/usernames")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(valid.request.body.clone().unwrap_or_default()))
            .expect("request")
    };

    for _ in 0..2 {
        let observed =
            Observed::from(app.clone().oneshot(request()).await.expect("response")).await;
        assert_eq!(observed.status, 500);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&observed.body_text).expect("json"),
            serde_json::json!({ "error": "Oops! Something went wrong." })
        );
    }

    let observed = Observed::from(app.clone().oneshot(request()).await.expect("response")).await;
    assert_eq!(observed.status, 429);
    assert_eq!(observed.retry_after.as_deref(), Some("60"));
    assert_eq!(
        observed.content_type.as_deref(),
        Some("application/problem+json")
    );
}
