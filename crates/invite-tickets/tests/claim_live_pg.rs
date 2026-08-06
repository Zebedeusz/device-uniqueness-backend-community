//! Live-Postgres composition tests for the claim path: the FIFO grab, the
//! frozen 200 body, and the concurrency semantics (exactly one winner; the
//! loser's 409; the emptied pool's 422).
//!
//! These need a real database, which the workspace deliberately keeps out of
//! CI, so they are `#[ignore]`d. Run them locally against any scratch
//! Postgres:
//!
//! ```sh
//! INVITE_TICKETS_TEST_DATABASE_URL=postgres://user:pass@localhost:5432/invite_test \
//!   cargo test -p invite-tickets -- --ignored --test-threads=1
//! ```
//!
//! The suite owns the `invite_tickets` table in that database (it runs the
//! crate's migrations and truncates between tests), so it must run
//! single-threaded: parallel tests truncate each other's fixtures.

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use invite_tickets::sign::{self, TicketKeypair};
use invite_tickets::tickets::{self, Dim, Network};
use invite_tickets::{AppState, Config};
use tower::ServiceExt as _;

const JWT_SEED: [u8; 32] = [42u8; 32];
const SUBJECT: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";
const VALID_WHO: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
const INVITER: &str = "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM";

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

async fn test_pool() -> sqlx::PgPool {
    let url = std::env::var("INVITE_TICKETS_TEST_DATABASE_URL")
        .expect("set INVITE_TICKETS_TEST_DATABASE_URL to run the live-PG tests");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect to test Postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    sqlx::query("TRUNCATE invite_tickets")
        .execute(&pool)
        .await
        .expect("truncate");
    pool
}

fn app_over(pool: sqlx::PgPool) -> axum::Router {
    let key = SigningKey::from_bytes(&JWT_SEED);
    let config = Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        database_url: "unused".to_string(),
        network: Network::Paseo,
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        rate_limit: 1000,
        rate_window: Duration::from_secs(60),
    };
    invite_tickets::routes(AppState::new(pool, config))
}

async fn seed_ticket(pool: &sqlx::PgPool, dim: Dim, network: Network) -> [u8; 32] {
    let seed = sign::generate_seed();
    let keypair = TicketKeypair::from_stored_secret(&seed).expect("valid seed");
    tickets::insert_available(pool, &keypair.public_bytes(), &seed, dim, network, INVITER)
        .await
        .expect("insert ticket");
    keypair.public_bytes()
}

fn claim_request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/invitation-ticket/claim")
        .header("authorization", format!("Bearer {}", mint_token()))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"who":"{VALID_WHO}","dim":"Game"}}"#
        )))
        .expect("request")
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&body).expect("json body")
}

#[tokio::test]
#[ignore = "requires Postgres; set INVITE_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn claims_the_oldest_ticket_with_the_frozen_body_and_a_verifying_signature() {
    let pool = test_pool().await;
    // Oldest first: the first-seeded ticket must win.
    let oldest = seed_ticket(&pool, Dim::Game, Network::Paseo).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _newer = seed_ticket(&pool, Dim::Game, Network::Paseo).await;
    // A different pool must never leak into this claim.
    let _other_pool = seed_ticket(&pool, Dim::ProofOfInk, Network::Paseo).await;

    let app = app_over(pool.clone());
    let response = app.oneshot(claim_request()).await.expect("response");
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;

    assert_eq!(body["publicKey"], format!("0x{}", hex::encode(oldest)));
    assert_eq!(body["inviter"], INVITER);
    assert_eq!(body["dim"], "Game");
    assert_eq!(body["network"], "paseo");
    assert_eq!(body["claimedBy"], VALID_WHO);
    assert_eq!(body["remaining"], 1);
    for field in ["createdAt", "claimedAt"] {
        let value = body[field].as_str().expect("ISO string");
        assert!(
            value.ends_with('Z') && value.len() == 24,
            "{field} must be JS-style ISO, got {value}"
        );
    }

    // The signature verifies over the decoded account id under the ticket key.
    let signature: [u8; 64] = hex::decode(
        body["signature"]
            .as_str()
            .expect("hex")
            .trim_start_matches("0x"),
    )
    .expect("hex")
    .try_into()
    .expect("64 bytes");
    let account =
        <subxt::utils::AccountId32 as std::str::FromStr>::from_str(VALID_WHO).expect("valid SS58");
    assert!(sign::verify(&oldest, &account.0, &signature));

    // The row flipped to claimed.
    let remaining = tickets::count_available(&pool, Dim::Game, Network::Paseo)
        .await
        .expect("count");
    assert_eq!(remaining, 1);
}

#[tokio::test]
#[ignore = "requires Postgres; set INVITE_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn concurrent_claims_on_one_ticket_yield_exactly_one_winner() {
    let pool = test_pool().await;
    seed_ticket(&pool, Dim::Game, Network::Paseo).await;
    let app = app_over(pool.clone());

    let (a, b) = tokio::join!(
        app.clone().oneshot(claim_request()),
        app.clone().oneshot(claim_request()),
    );
    let mut statuses = [
        a.expect("response").status().as_u16(),
        b.expect("response").status().as_u16(),
    ];
    statuses.sort_unstable();
    // Exactly one winner; the loser's status depends on interleaving, in
    // legacy exactly as here: 409 when both pre-checks passed before either
    // commit (the row race), 422 when the winner committed before the loser's
    // pre-check (an already-empty pool). The deterministic 409 path is pinned
    // separately below.
    assert_eq!(statuses[0], 200);
    assert!(
        statuses[1] == 409 || statuses[1] == 422,
        "loser must be 409 or 422, got {}",
        statuses[1]
    );

    // Pool now empty: the next claim is the 422, with the frozen body.
    let response = app.oneshot(claim_request()).await.expect("response");
    assert_eq!(response.status(), 422);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "error": "Pool exhausted" })
    );
}

#[tokio::test]
#[ignore = "requires Postgres; set INVITE_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn race_loser_body_is_the_frozen_409() {
    let pool = test_pool().await;
    seed_ticket(&pool, Dim::Game, Network::Paseo).await;

    // Force the race deterministically: hold the only row's lock in an open
    // transaction, then claim — SKIP LOCKED sees nothing after a non-empty
    // pre-check, which is the frozen 409 (even though the pool "exists").
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("SELECT public_key FROM invite_tickets FOR UPDATE")
        .fetch_all(&mut *tx)
        .await
        .expect("lock rows");

    let app = app_over(pool.clone());
    let response = app.oneshot(claim_request()).await.expect("response");
    assert_eq!(response.status(), 409);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "error": "Ticket race lost" })
    );
    tx.rollback().await.expect("rollback");
}
