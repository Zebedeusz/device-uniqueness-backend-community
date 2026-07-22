//! Golden legacy-fixture replay: pins the Rust wire contract against
//! request/response pairs captured from the REAL legacy TypeScript route
//! (`fixtures/legacy-wire.json`; provenance in `fixtures/README.md`).
//!
//! Two altitudes, matching what each fixture needs:
//! - **Router replay** — every case whose path reaches neither Postgres nor a
//!   live chain read (the 401s, the whole 400 validation matrix, malformed
//!   JSON, the missing-content-type 500, the SS58-quirk 500, the 404
//!   fallbacks) is driven through the full production router offline and
//!   compared status/content-type/`Retry-After`/body against the capture. The
//!   router is built over a dead lazy Postgres pool and an offline chain
//!   client (mock RPC that answers only the genesis-hash handshake), so the
//!   two `shell-defect` 500s replay through the router too: any path that
//!   would touch DB or chain fails exactly like the legacy shell defect did —
//!   with the opaque global-hook body.
//! - **Response replay** — cases that need real DB/chain state on the request
//!   path (the 200 bodies, 409, 422, the GET 404) compare the exact response
//!   the handler would emit (`AppError` rendering, the `tickets` response
//!   builders over the capture's stub values).
//!
//! The 429 rate-limit dialect is pinned here without a captured fixture (the
//! capture did not mount the legacy limiter; its body is transcribed from the
//! legacy middleware source, as with invite-tickets).
//!
//! JWTs: the legacy capture authenticated with HS256 (the legacy secret);
//! this service verifies identity-service Ed25519 tokens, so authenticated
//! replays mint an EdDSA token for a test key.
//!
//! Dynamic fields: the legacy route stamps `updatedAt` with "now" on POST 200
//! bodies and on PENDING GET bodies, so those two captures carry a
//! capture-time value — they compare with `updatedAt` masked, plus a shape
//! assertion (JS-style ISO millis).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use dim_tickets::http::error::AppError;
use dim_tickets::tickets::{create_response, status_response, Dim, Network, TicketRow};
use dim_tickets::{AppState, ChainClient, Config};
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use time::macros::datetime;
use tower::ServiceExt as _;

const FIXTURES: &str = include_str!("fixtures/legacy-wire.json");

const JWT_SEED: [u8; 32] = [42u8; 32];
const SUBJECT: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

/// The capture's stub values (see `fixtures/capture/…golden-capture.test.ts`).
const INVITER: &str = "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM";
const ADDR_CREATE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
const ADDR_EXISTS: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
const ADDR_QUOTA: &str = "5FLSigC9HGRKVhB9FiEo4Y3koPsNmBmLJbpXg2mp1hXcS59Y";
const ADDR_DIE: &str = "5DAAnrj7VHTznn2AWBemMuyBwZWs6FNFjdyVXUeYum3PTXFy";
const ADDR_FAILED: &str = "5HpG9w8EBLe5XCrbczpwq5TSXvedjrBGCwqxK1iQ7qUsSWFc";

const CREATED_AT: time::OffsetDateTime = datetime!(2026-07-01 10:20:30.400 UTC);
const STATE_AT: time::OffsetDateTime = datetime!(2026-07-02 11:00:00 UTC);

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
/// RPC that answers only the genesis-hash handshake. Any actual read (the
/// quota check) fails — which is exactly what the offline replay wants: the
/// same opaque 500 the legacy shell defect produced.
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

/// A production router over a lazy pool that connects to nothing and the
/// offline chain client: no replayed offline fixture may complete a DB or
/// chain read (doing so fails loudly as a 500 mismatch).
async fn offline_app(rate_limit: u32) -> axum::Router {
    let key = SigningKey::from_bytes(&JWT_SEED);
    let config = Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        database_url: "unused".to_string(),
        people_rpc_url: "unused".to_string(),
        network: Network::Paseo,
        inviter_address: INVITER.to_string(),
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        quota_timeout: Duration::from_secs(1),
        rate_limit,
        rate_window: Duration::from_secs(60),
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://nobody@127.0.0.1:1/nothing")
        .expect("lazy pool");
    dim_tickets::routes(AppState::new(pool, dead_chain_client().await, config))
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

/// Fixture names that must replay through the full router offline.
fn is_offline(name: &str) -> bool {
    !matches!(
        name,
        "post-200-create"
            | "post-200-create-proofofink"
            | "post-409-already-exists"
            | "post-422-quota-exceeded"
            | "get-200-pending"
            | "get-200-submitting"
            | "get-200-submitted"
            | "get-200-registered"
            | "get-200-failed"
            | "get-404-not-found"
    )
}

#[tokio::test]
async fn offline_fixtures_replay_through_the_production_router() {
    let app = offline_app(1000).await;
    let token = mint_token();
    let mut replayed = 0;

    for fixture in fixtures() {
        if !is_offline(&fixture.name) {
            continue;
        }
        let mut builder = Request::builder()
            .method(fixture.request.method.as_str())
            .uri(&fixture.request.path);
        for (name, value) in &fixture.request.headers {
            // The capture authenticated with a legacy HS256 token; replay
            // with a minted EdDSA one. Deliberately-broken auth headers
            // (`Basic abc`, `Bearer not-a-jwt`) replay verbatim.
            if name.eq_ignore_ascii_case("authorization") && value.starts_with("Bearer eyJ") {
                builder = builder.header(name, format!("Bearer {token}"));
            } else {
                builder = builder.header(name, value);
            }
        }
        let body = fixture.request.body.clone().unwrap_or_default();
        let request = builder.body(Body::from(body)).expect("request");

        let response = app.clone().oneshot(request).await.expect("response");
        Observed::from(response)
            .await
            .assert_matches(&fixture.name, &fixture.response);
        replayed += 1;
    }
    // The matrix is frozen: a shrinking fixture file must fail loudly.
    assert_eq!(replayed, 19, "offline fixture count changed");
}

/// A ticket row shaped like the capture's stub (see the capture harness).
fn stub_row(ticket: &str, status: &str, onchain_data: Option<serde_json::Value>) -> TicketRow {
    TicketRow {
        ticket: ticket.to_string(),
        inviter: INVITER.to_string(),
        network: "paseo".to_string(),
        dim: "Game".to_string(),
        status: status.to_string(),
        onchain_data,
        created_at: CREATED_AT,
        updated_at: Some(STATE_AT),
    }
}

/// Compare a built body against a captured one whose `updatedAt` is a
/// capture-time stamp (POST 200s and the PENDING GET): everything else must
/// match exactly and both `updatedAt`s must be JS-style ISO millis.
fn assert_matches_with_dynamic_updated_at(
    name: &str,
    mut got: serde_json::Value,
    fixture: &Fixture,
) {
    let mut want = fixture.response.body.clone();
    for value in [&mut got, &mut want] {
        let updated_at = value
            .as_object_mut()
            .expect("object body")
            .remove("updatedAt")
            .expect("updatedAt present");
        let text = updated_at.as_str().expect("updatedAt string");
        assert!(
            text.ends_with('Z') && text.len() == 24,
            "{name}: updatedAt must be JS-style ISO millis, got {text}"
        );
    }
    assert_eq!(got, want, "{name}: body (updatedAt masked)");
}

#[tokio::test]
async fn create_200_bodies_match_the_capture() {
    for (name, dim) in [
        ("post-200-create", Dim::Game),
        ("post-200-create-proofofink", Dim::ProofOfInk),
    ] {
        let fixture = find_fixture(name);
        assert_eq!(fixture.response.status, 200, "{name}");
        let mut row = stub_row(ADDR_CREATE, "PENDING", None);
        row.dim = dim.as_str().to_string();
        // The response's `updatedAt` is "now"; the capture's is capture-time.
        let body = create_response(&row, time::OffsetDateTime::now_utc());
        assert_matches_with_dynamic_updated_at(name, body, &fixture);
    }
}

#[tokio::test]
async fn status_200_bodies_match_the_capture() {
    // PENDING reports `updatedAt: now` (legacy quirk) — masked compare.
    let fixture = find_fixture("get-200-pending");
    let body = status_response(
        &stub_row(ADDR_CREATE, "PENDING", None),
        time::OffsetDateTime::now_utc(),
    );
    assert_matches_with_dynamic_updated_at("get-200-pending", body, &fixture);

    // Every other status echoes its state-change timestamp — exact compare.
    let onchain = serde_json::json!({
        "blockIndex": "12345-7",
        "blockNumber": 12345,
        "blockHash": format!("0x{}", "ab".repeat(32)),
        "eventIndex": 7,
    });
    for (name, ticket, status, onchain_data) in [
        ("get-200-submitting", ADDR_EXISTS, "SUBMITTING", None),
        ("get-200-submitted", ADDR_QUOTA, "SUBMITTED", None),
        ("get-200-registered", ADDR_DIE, "REGISTERED", Some(onchain)),
        ("get-200-failed", ADDR_FAILED, "FAILED", None),
    ] {
        let fixture = find_fixture(name);
        assert_eq!(fixture.response.status, 200, "{name}");
        let body = status_response(
            &stub_row(ticket, status, onchain_data),
            time::OffsetDateTime::now_utc(),
        );
        assert_eq!(body, fixture.response.body, "{name}");
    }
}

#[tokio::test]
async fn handler_error_renderings_match_the_capture() {
    for (name, error) in [
        ("post-409-already-exists", AppError::TicketAlreadyExists),
        ("post-422-quota-exceeded", AppError::NoAvailableInvites),
        ("get-404-not-found", AppError::TicketNotFound),
    ] {
        let fixture = find_fixture(name);
        let observed = Observed::from(error.into_response()).await;
        observed.assert_matches(name, &fixture.response);
    }
}

// --- pinned without a capture (see fixtures/README.md) ---------------------

#[tokio::test]
async fn rate_limit_returns_the_transcribed_429_after_the_window_fills() {
    // Limit 2 on the GET route: two requests pass the limiter (and then die
    // at the unreachable DB — the legacy opaque 500), the third is refused
    // with Retry-After.
    let app = offline_app(2).await;
    let token = mint_token();
    let request = || {
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/dim-ticket/{ADDR_CREATE}"))
            .header("authorization", format!("Bearer {token}"))
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
