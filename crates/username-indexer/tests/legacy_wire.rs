//! Golden legacy-fixture replay: pins the Rust wire contract against
//! request/response pairs captured from the REAL legacy TypeScript usernames
//! router (`fixtures/legacy-wire.json`; provenance in `fixtures/README.md`).
//!
//! The 48 captured fixtures split into three frozen groups:
//! - **Offline router replay** — every case whose path reaches no database
//!   (the search 400 validation matrix, the cursor 400s, the OPTIONS/404
//!   fallbacks) drives the full production router over a dead lazy pool and
//!   compares status/content-type/`Retry-After`/body against the capture.
//! - **Dropped endpoints** — the deprecated list endpoint's 200s (Sunset
//!   passed) AND every `GET /{username}` lookup capture (endpoint removed
//!   2026-07-23, deprecated in favour of paginated `search`): this service
//!   deliberately serves the Hono `404 Not Found` on both. Replayed against
//!   the router asserting the DIVERGENT response; the captures are retained
//!   as evidence of the retired wire contract.
//! - **Live-DB parity** — the search 200s and the search header probes need
//!   real rows; their envelope/date/digit-render shapes are pinned by unit
//!   tests in `src/search.rs` and by the manual `pagination_live` test.
//!
//! The 429 dialect is pinned without a captured fixture (the capture ran the
//! rate limiters as pass-through probes; the body is transcribed from the
//! legacy middleware source, as in every other crate).

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use serde::Deserialize;
use tower::ServiceExt as _;
use username_indexer::http::middleware::RateLimiter;
use username_indexer::{AppState, ChainClient, Freshness};

const FIXTURES: &str = include_str!("fixtures/legacy-wire.json");

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

fn find_fixture(name: &str) -> Fixture {
    fixtures()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fixture {name} exists"))
}

/// An offline People Chain client (mock RPC answering only the genesis-hash
/// handshake). The replayed routes never read the chain; only `AppState`
/// construction needs it.
async fn dead_chain_client() -> ChainClient {
    use std::sync::Arc;

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
/// replayed offline fixture may complete a DB read (doing so fails loudly as
/// a 500 mismatch).
async fn offline_app(rate_limit: u32) -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://nobody@127.0.0.1:1/nothing")
        .expect("lazy pool");
    username_indexer::routes(AppState::new(
        pool,
        dead_chain_client().await,
        Freshness::new(),
        RateLimiter::new(rate_limit, Duration::from_secs(60)),
    ))
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

async fn replay(app: &axum::Router, fixture: &Fixture) -> Observed {
    let mut builder = Request::builder()
        .method(fixture.request.method.as_str())
        .uri(&fixture.request.path);
    for (name, value) in &fixture.request.headers {
        builder = builder.header(name, value);
    }
    let body = fixture.request.body.clone().unwrap_or_default();
    let request = builder.body(Body::from(body)).expect("request");
    Observed::from(app.clone().oneshot(request).await.expect("response")).await
}

/// Fixtures replayed through the full production router offline, expecting a
/// byte-frozen match.
const OFFLINE_ROUTER: &[&str] = &[
    "search-400-limit-zero",
    "search-400-limit-negative",
    "search-400-limit-nan",
    "search-400-limit-fractional",
    "search-400-prefix-missing",
    "search-400-prefix-empty",
    "search-400-prefix-too-long",
    "search-400-prefix-space",
    "search-400-prefix-hyphen",
    "search-400-prefix-double-dot",
    "search-400-multi-failure",
    "search-400-cursor-not-base64",
    "search-400-cursor-wrong-json",
    // No CORS anywhere on this surface (the legacy cors() bound only the
    // collection root, which serves the dropped list endpoint): preflights
    // fall through to the frozen Hono 404, exactly as captured.
    "search-cors-preflight",
    "get-cors-preflight",
    "get-404-unknown-subpath",
    "put-404-search",
    // Legacy itself 404s the trailing-slash collection path.
    "list-200-evidence-trailing-slash",
];

/// Captures of the two deliberately-dropped endpoints, retained as evidence:
/// the deprecated list endpoint (its Sunset date passed) and the whole
/// `GET /{username}` lookup surface (removed 2026-07-23, deprecated in
/// favour of paginated `search` — includes the RESERVED-row divergence
/// evidence and the lookup header probes). The router must answer the frozen
/// Hono 404 on every one of these requests.
const DROPPED_ENDPOINT: &[&str] = &[
    "list-200-evidence-prefix",
    "list-200-evidence-no-prefix",
    "get-400-digits-00",
    "get-400-uppercase",
    "get-400-short-base",
    "get-400-one-digit",
    "get-400-three-digits",
    "get-400-bare-base",
    "get-404-not-found",
    "get-200-assigned",
    "get-200-reserved-evidence",
    "get-auth-probe-bad-bearer",
    "get-cors-probe-origin",
    "get-etag-first",
    "get-etag-if-none-match",
];

/// Search 200s and header probes need live projection rows; their envelope,
/// JS-ISO dates, integer digit render, clamp, and unknown-param behavior are
/// pinned by `src/search.rs` unit tests and the manual live pagination test.
const DB_PARITY_VIA_LIVE: &[&str] = &[
    "search-200-empty",
    "search-200-mixed-order",
    "search-200-include-onchain-true",
    "search-200-include-onchain-false-string",
    "search-200-uppercase-prefix",
    "search-200-lite-only-dotted-prefix",
    "search-200-limit-clamped",
    "search-200-page1-limit1",
    "search-200-page2-limit1",
    "search-200-page3-limit2-final",
    "search-200-unknown-param-ignored",
    "search-auth-probe-bad-bearer",
    "search-cors-probe-origin",
    "search-etag-first",
    "search-etag-if-none-match",
];

#[test]
fn every_fixture_is_classified_exactly_once() {
    let all = fixtures();
    let mut classified: Vec<&str> = [OFFLINE_ROUTER, DROPPED_ENDPOINT, DB_PARITY_VIA_LIVE].concat();
    classified.sort_unstable();
    let duplicates = classified.windows(2).any(|pair| pair[0] == pair[1]);
    assert!(!duplicates, "a fixture is classified twice");
    let mut names: Vec<String> = all.iter().map(|f| f.name.clone()).collect();
    names.sort_unstable();
    let mut expected: Vec<String> = classified.iter().map(|s| s.to_string()).collect();
    expected.sort_unstable();
    assert_eq!(names, expected, "fixture set and classification diverged");
    assert_eq!(all.len(), 48, "captured fixture count is frozen");
}

#[tokio::test]
async fn offline_fixtures_replay_through_the_production_router() {
    let app = offline_app(1000).await;
    for name in OFFLINE_ROUTER {
        let fixture = find_fixture(name);
        replay(&app, &fixture)
            .await
            .assert_matches(name, &fixture.response);
    }
}

#[tokio::test]
async fn dropped_endpoints_serve_the_hono_404() {
    let app = offline_app(1000).await;
    for name in DROPPED_ENDPOINT {
        let fixture = find_fixture(name);
        let observed = replay(&app, &fixture).await;
        assert_eq!(observed.status, 404, "{name}: dropped endpoint must 404");
        assert_eq!(
            observed.content_type.as_deref(),
            Some("text/plain; charset=UTF-8"),
            "{name}: Hono 404 content type"
        );
        assert_eq!(observed.body_text, "404 Not Found", "{name}: Hono 404 body");
    }
}

// --- pinned without a capture (see fixtures/README.md) ---------------------

#[tokio::test]
async fn rate_limit_returns_the_transcribed_429_after_the_window_fills() {
    // Limit 2: two requests pass the limiter (and then die at the
    // unreachable DB — the legacy opaque 500), the third is refused.
    let app = offline_app(2).await;
    let request = || {
        Request::builder()
            .method("GET")
            .uri("/api/v1/usernames/search?prefix=alice")
            .header("x-forwarded-for", "203.0.113.7")
            .body(Body::empty())
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
}
