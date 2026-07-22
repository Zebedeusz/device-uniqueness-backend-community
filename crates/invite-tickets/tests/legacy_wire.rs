//! Golden legacy-fixture replay: pins the Rust wire contract against
//! request/response pairs captured from the REAL legacy TypeScript route
//! (`fixtures/legacy-wire.json`; provenance in `fixtures/README.md`).
//!
//! Two altitudes, matching what each fixture needs:
//! - **Router replay** — every case whose path never touches Postgres (the
//!   401 trio, the whole 400 validation matrix incl. the SS58 refine and the
//!   non-aborting empty-`who` double error, malformed JSON, the
//!   missing-content-type 500, the 404 fallbacks) is driven through the full
//!   production router (`http::routes`) offline and compared
//!   status/content-type/`Retry-After`/body against the capture.
//! - **Response replay** — cases that need DB state on the request path (the
//!   200 bodies, 409, 422, the shell-defect 500) compare the exact response
//!   the handler would emit (`AppError` rendering, `tickets::claim_response`
//!   over the capture's stub values). Their full-path composition runs in
//!   `tests/claim_live_pg.rs` against live Postgres.
//!
//! Two legacy behaviors are pinned here without a captured fixture (see the
//! fixtures README): the 429 rate-limit dialect (transcribed from the legacy
//! middleware source) and expired-token 401s (time-dependent).
//!
//! JWTs: the legacy capture authenticated with HS256 (the legacy secret);
//! this service verifies identity-service Ed25519 tokens, so authenticated
//! replays mint an EdDSA token for a test key.

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use invite_tickets::http::error::AppError;
use invite_tickets::tickets::{claim_response, ClaimedTicket, Dim, Network};
use invite_tickets::{AppState, Config};
use serde::Deserialize;
use time::macros::datetime;
use tower::ServiceExt as _;

const FIXTURES: &str = include_str!("fixtures/legacy-wire.json");

const JWT_SEED: [u8; 32] = [42u8; 32];
const SUBJECT: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

/// The capture's stub values (see `fixtures/capture/…golden-capture.test.ts`).
const INVITER: &str = "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM";
const ADDR_OK: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";

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

/// A production router over a lazy pool that connects to nothing: no replayed
/// offline fixture may reach the DB (doing so fails loudly as a 500 mismatch).
fn offline_app(rate_limit: u32) -> axum::Router {
    let key = SigningKey::from_bytes(&JWT_SEED);
    let config = Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        database_url: "unused".to_string(),
        network: Network::Paseo,
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        rate_limit,
        rate_window: Duration::from_secs(60),
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://nobody@127.0.0.1:1/nothing")
        .expect("lazy pool");
    invite_tickets::routes(AppState::new(pool, config))
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

/// Fixture names that must replay through the full router without a database.
fn is_offline(name: &str) -> bool {
    !matches!(
        name,
        "post-200-claim"
            | "post-200-claim-proofofink"
            | "post-422-pool-exhausted"
            | "post-409-ticket-race"
            | "post-500-shell-defect"
    )
}

#[tokio::test]
async fn offline_fixtures_replay_through_the_production_router() {
    let app = offline_app(1000);
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
    assert_eq!(replayed, 17, "offline fixture count changed");
}

/// The capture's stubbed `ClaimResult`, as this service's row + signature.
fn stub_ticket() -> (ClaimedTicket, [u8; 64]) {
    (
        ClaimedTicket {
            public_key: vec![0xab; 32],
            private_key: Vec::new(),
            inviter: INVITER.to_string(),
            created_at: datetime!(2026-07-01 10:20:30.400 UTC),
        },
        [0xcd; 64],
    )
}

#[tokio::test]
async fn claim_200_bodies_match_the_capture() {
    for (name, dim) in [
        ("post-200-claim", Dim::Game),
        ("post-200-claim-proofofink", Dim::ProofOfInk),
    ] {
        let fixture = fixtures()
            .into_iter()
            .find(|f| f.name == name)
            .expect("fixture exists");
        let (ticket, signature) = stub_ticket();
        let body = claim_response(
            &ticket,
            &signature,
            dim,
            Network::Paseo,
            ADDR_OK,
            datetime!(2026-07-02 11:00:00 UTC),
            41,
        );
        assert_eq!(body, fixture.response.body, "{name}");
        assert_eq!(fixture.response.status, 200, "{name}");
    }
}

#[tokio::test]
async fn handler_error_renderings_match_the_capture() {
    for (name, error) in [
        ("post-422-pool-exhausted", AppError::PoolExhausted),
        ("post-409-ticket-race", AppError::TicketRaceLost),
        (
            "post-500-shell-defect",
            AppError::Internal(anyhow::anyhow!("simulated shell defect")),
        ),
    ] {
        let fixture = fixtures()
            .into_iter()
            .find(|f| f.name == name)
            .expect("fixture exists");
        let observed = Observed::from(error.into_response()).await;
        observed.assert_matches(name, &fixture.response);
    }
}

// --- pinned without a capture (see fixtures/README.md) ---------------------

#[tokio::test]
async fn expired_token_is_the_verification_401() {
    let app = offline_app(1000);
    let key = SigningKey::from_bytes(&JWT_SEED);
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"accountId":"{SUBJECT}","sub":"{SUBJECT}","exp":1000000}}"#
    ));
    let payload = format!("{header}.{claims}");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(payload.as_bytes()).to_bytes());
    let token = format!("{payload}.{signature}");

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/invitation-ticket/claim")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("request");
    let observed = Observed::from(app.oneshot(request).await.expect("response")).await;
    assert_eq!(observed.status, 401);
    let body: serde_json::Value = serde_json::from_str(&observed.body_text).expect("json");
    assert_eq!(body["title"], "Invalid Token");
}

#[tokio::test]
async fn rate_limit_returns_the_transcribed_429_after_the_window_fills() {
    // Limit 2: two well-formed requests pass the limiter (and then die at the
    // unreachable DB — the legacy opaque 500), the third is refused with
    // Retry-After. Proves the limiter sits after validation (legacy order)
    // and before any DB work.
    let app = offline_app(2);
    let token = mint_token();
    let body = format!(r#"{{"who":"{ADDR_OK}","dim":"Game"}}"#);
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/invitation-ticket/claim")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
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

    // Invalid bodies 400 before the limiter — they never consume the window.
    let bad = Request::builder()
        .method("POST")
        .uri("/api/v1/invitation-ticket/claim")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("request");
    let observed = Observed::from(app.oneshot(bad).await.expect("response")).await;
    assert_eq!(observed.status, 400);
}
