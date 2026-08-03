//! Live end-to-end coverage for the payment lane at the HTTP boundary: the
//! production router over a real Postgres and the live People Chain RPC.
//! Proves the Phase-2 demo journey — a hard-mode iOS claim with **no device
//! token** (the spec's "unrecognized device tokens simply resolve to
//! PAYMENT_REQUIRED") gets `200` deposit instructions and a durable PENDING
//! request; a re-claim gets the same address back.

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use secrecy::SecretString;
use sqlx::Row as _;
use tower::ServiceExt as _;

use identity_service::config::{DeviceCheckConfig, PaymentConfig};
use identity_service::eligibility;
use identity_service::{AppState, ChainClient, Config, Jwt};

const JWT_SEED: [u8; 32] = [7u8; 32];
const SUBJECT: &str = "0xpaymenthttplive";
const POP_REGISTER_PREFIX: &[u8] = b"pop:people-lite:register using";

/// Mint an EdDSA access token carrying the iOS `plt` claim (the device gate
/// only runs for iOS subjects).
fn mint_ios_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs() as i64
        + 3600;
    let key = SigningKey::from_bytes(&JWT_SEED);
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"accountId":"{SUBJECT}","sub":"{SUBJECT}","plt":"ios","exp":{exp}}}"#
    ));
    let payload = format!("{header}.{claims}");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(payload.as_bytes()).to_bytes());
    format!("{payload}.{signature}")
}

/// A throwaway (but structurally valid) DeviceCheck config: the missing-token
/// path never calls Apple, but `AppState` requires a parseable EC key.
fn dummy_device_check() -> DeviceCheckConfig {
    use p256::pkcs8::EncodePrivateKey as _;
    let key = p256::SecretKey::random(&mut rand::rngs::OsRng);
    let pem = key
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("pem")
        .to_string();
    DeviceCheckConfig {
        team_id: "TESTTEAM".to_string(),
        key_id: "TESTKEY".to_string(),
        private_key_pem: SecretString::from(pem),
        base_url: "https://api.devicecheck.apple.com/v1".to_string(),
    }
}

fn claim_body(base: &str) -> serde_json::Value {
    use std::str::FromStr as _;
    use subxt_signer::sr25519::Keypair;
    use subxt_signer::SecretUri;

    let keypair = Keypair::from_uri(&SecretUri::from_str("//Alice").unwrap()).unwrap();
    let candidate = keypair.public_key();
    let ring = [7u8; 32];
    let mut message = Vec::new();
    message.extend_from_slice(POP_REGISTER_PREFIX);
    message.extend_from_slice(&candidate.0);
    message.extend_from_slice(&ring);
    let signature = keypair.sign(&message).0;

    serde_json::json!({
        "candidateAccountId": candidate.to_account_id().to_string(),
        "username": base,
        "candidateSignature": format!("0x{}", hex::encode(signature)),
        "ringVrfKey": format!("0x{}", hex::encode(ring)),
        "proofOfOwnership": format!("0x{}", "03".repeat(64)),
        "consumerRegistrationSignature": format!("0x{}", "04".repeat(64)),
        "identifierKey": format!("0x{}", "05".repeat(65))
    })
}

async fn post_claim(
    app: &axum::Router,
    token: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/usernames")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)));
    (status, json)
}

fn unique_base(prefix: &str) -> String {
    let letters: String = std::process::id()
        .to_string()
        .bytes()
        .map(|b| (b'a' + (b - b'0')) as char)
        .collect();
    format!("{prefix}{letters}")
}

#[tokio::test]
#[ignore = "requires Postgres (IDENTITY_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn missing_device_token_resolves_to_a_payment_quote_over_the_production_router() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
        .bind(SUBJECT)
        .execute(&pool)
        .await
        .expect("pre-clean");
    let rpc_url = std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
    let chain = ChainClient::connect(&rpc_url).await.expect("live RPC");

    let mut config = Config::test_default();
    // Hard mode + DeviceCheck on: a missing token is `TokenRequired`; the
    // payment lane converts it into the quote outcome (no Apple call on this
    // path). Queue on to show the payment lane pre-empts it.
    config.enforce_auth = true;
    config.queue_enabled = true;
    config.device_check = Some(dummy_device_check());
    let payment = PaymentConfig {
        master_account: [7u8; 32],
        amount_planck: 10_000_000_000,
        request_ttl: Duration::from_secs(3600),
    };
    config.payment = Some(payment.clone());
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let app = identity_service::routes(AppState::new(pool.clone(), chain, jwt, config));
    let token = mint_ios_token();

    let base = unique_base("payhttp");

    // Claim with no Device-Token-iOS header → deposit instructions.
    let (status, body) = post_claim(&app, &token, &claim_body(&base)).await;
    assert_eq!(status, 200, "payment quote: {body}");
    assert_eq!(body["registrationOutcome"], "PAYMENT_REQUIRED", "{body}");
    assert_eq!(
        body["amountRequired"],
        payment.amount_planck.to_string(),
        "{body}"
    );
    let address = body["paymentAddress"]
        .as_str()
        .expect("address")
        .to_string();

    // The durable request stores the claim intent.
    let row = sqlx::query(
        "SELECT status, base, payment_address FROM payment_requests WHERE account_id = $1",
    )
    .bind(SUBJECT)
    .fetch_one(&pool)
    .await
    .expect("payment request row");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "PENDING");
    assert_eq!(row.try_get::<String, _>("base").unwrap(), base);
    assert_eq!(
        row.try_get::<String, _>("payment_address").unwrap(),
        address
    );

    // Re-claim: the same quote back (idempotent), not a second address.
    let (status, body) = post_claim(&app, &token, &claim_body(&base)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["paymentAddress"], address.as_str(), "{body}");

    // The payment-status poll (spec-exact two-state body): PENDING while the
    // deposit is unseen, CONFIRMED once the request is confirmed, 404 with
    // no active request.
    let (status, body) = get_payment_status(&app, &token).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({ "status": "PENDING" }),
        "spec-exact PENDING body"
    );

    let request_id: i64 =
        sqlx::query("SELECT id FROM payment_requests WHERE account_id = $1 ORDER BY id DESC")
            .bind(SUBJECT)
            .fetch_one(&pool)
            .await
            .expect("row")
            .try_get("id")
            .unwrap();
    let chain2 = ChainClient::connect(&rpc_url).await.expect("live RPC");
    identity_service::payment::confirm_by_id(&pool, &chain2, request_id)
        .await
        .expect("confirm")
        .expect("was pending");
    let (status, body) = get_payment_status(&app, &token).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({ "status": "CONFIRMED" }),
        "spec-exact CONFIRMED body"
    );

    sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
        .bind(SUBJECT)
        .execute(&pool)
        .await
        .expect("clean payment requests");
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean reservations");

    // No request left → 404 JSON.
    let (status, body) = get_payment_status(&app, &token).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], "No active payment request", "{body}");
}

/// Mint a token for `subject` with an explicit `appFromOfficialStore`
/// verdict (Android — no DeviceCheck involvement).
fn mint_android_token(subject: &str, official_store: bool) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs() as i64
        + 3600;
    let key = SigningKey::from_bytes(&JWT_SEED);
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"accountId":"{subject}","sub":"{subject}","plt":"android","appFromOfficialStore":{official_store},"exp":{exp}}}"#
    ));
    let payload = format!("{header}.{claims}");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(payload.as_bytes()).to_bytes());
    format!("{payload}.{signature}")
}

#[tokio::test]
#[ignore = "requires Postgres (IDENTITY_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn non_store_install_routes_to_payment_and_store_install_proceeds() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let vanilla_subject = "0xvanillaandroid";
    let store_subject = "0xstoreandroid";
    let vanilla_base = unique_base("vanapk");
    let store_base = unique_base("storeapk");
    for subject in [vanilla_subject, store_subject] {
        sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
            .bind(subject)
            .execute(&pool)
            .await
            .expect("pre-clean");
    }
    let rpc_url = std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
    let chain = ChainClient::connect(&rpc_url).await.expect("live RPC");

    // Payment lane on; no DeviceCheck, no hard mode — the official-store
    // verdict alone must route (spec FR-005: no device-identity check).
    let mut config = Config::test_default();
    config.payment = Some(PaymentConfig {
        master_account: [7u8; 32],
        amount_planck: 10_000_000_000,
        request_ttl: Duration::from_secs(3600),
    });
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let app = identity_service::routes(AppState::new(pool.clone(), chain, jwt, config));

    // Vanilla APK (appFromOfficialStore=false) → the payment quote.
    let vanilla = mint_android_token(vanilla_subject, false);
    let (status, body) = post_claim(&app, &vanilla, &claim_body(&vanilla_base)).await;
    assert_eq!(status, 200, "vanilla claim: {body}");
    assert_eq!(body["registrationOutcome"], "PAYMENT_REQUIRED", "{body}");
    assert!(body["paymentAddress"].is_string(), "{body}");

    // Store install (appFromOfficialStore=true) → the normal 202 reservation.
    let store = mint_android_token(store_subject, true);
    let (status, body) = post_claim(&app, &store, &claim_body(&store_base)).await;
    assert_eq!(status, 202, "store claim: {body}");
    assert_eq!(body["base_username"], store_base.as_str(), "{body}");

    for subject in [vanilla_subject, store_subject] {
        sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
            .bind(subject)
            .execute(&pool)
            .await
            .expect("clean payment requests");
    }
    for base in [&vanilla_base, &store_base] {
        sqlx::query("DELETE FROM username_reservations WHERE base = $1")
            .bind(base)
            .execute(&pool)
            .await
            .expect("clean reservations");
    }
}

#[tokio::test]
#[ignore = "requires Postgres (IDENTITY_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn a_valid_voucher_beats_the_non_store_payment_gate() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let subject = "0xvoucherbeatsfr005";
    let base = unique_base("vbeatsapk");
    sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
        .bind(subject)
        .execute(&pool)
        .await
        .expect("pre-clean");
    let rpc_url = std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
    let chain = ChainClient::connect(&rpc_url).await.expect("live RPC");

    // Both lanes on: voucher precedence (spec FR-009) must resolve the claim
    // before the FR-005 official-store gate ever routes to payment.
    let mut config = Config::test_default();
    config.registration_vouchers_enabled = true;
    config.payment = Some(PaymentConfig {
        master_account: [7u8; 32],
        amount_planck: 10_000_000_000,
        request_ttl: Duration::from_secs(3600),
    });
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let app = identity_service::routes(AppState::new(pool.clone(), chain, jwt, config));

    let batch = base.clone();
    let key = format!("fr005-key-{}", std::process::id());
    sqlx::query(
        "INSERT INTO registration_vouchers (key_hash, minted_batch, expires_at) \
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(eligibility::key_hash(&key))
    .bind(&batch)
    .execute(&pool)
    .await
    .expect("mint voucher");

    // Non-store install (appFromOfficialStore=false) carrying a valid
    // voucher: INSTANT, not PAYMENT_REQUIRED.
    let token = mint_android_token(subject, false);
    let mut body = claim_body(&base);
    body["lifetimePoUDVoucher"] = serde_json::json!(key);
    let (status, body) = post_claim(&app, &token, &body).await;
    assert_eq!(status, 202, "voucher claim: {body}");
    assert_eq!(body["registrationOutcome"], "INSTANT", "{body}");

    // The payment lane was never consulted: no quote row minted.
    let quotes: i64 =
        sqlx::query("SELECT count(*) AS n FROM payment_requests WHERE account_id = $1")
            .bind(subject)
            .fetch_one(&pool)
            .await
            .expect("count")
            .try_get("n")
            .unwrap();
    assert_eq!(quotes, 0, "a voucher claim must not mint a payment quote");

    sqlx::query("DELETE FROM registration_vouchers WHERE minted_batch = $1")
        .bind(&batch)
        .execute(&pool)
        .await
        .expect("clean vouchers");
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean reservations");
}

async fn get_payment_status(app: &axum::Router, token: &str) -> (u16, serde_json::Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/usernames/payment-status")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)));
    (status, json)
}
