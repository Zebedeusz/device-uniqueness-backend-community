//! Live end-to-end coverage for the voucher INSTANT lane at the HTTP
//! boundary: the production router over a real Postgres and the live People
//! Chain RPC. Proves the Phase-1 demo journey — mint → claim with voucher →
//! `202 {registrationOutcome: "INSTANT"}` + a `RESERVED` outbox row → reuse
//! rejected `400` — through the exact code path the deployed `identity-api`
//! serves.

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt as _;
use sqlx::Row as _;
use tower::ServiceExt as _;

use identity_service::eligibility;
use identity_service::{AppState, ChainClient, Config, Jwt};

const JWT_SEED: [u8; 32] = [7u8; 32];
const SUBJECT: &str = "0xvoucherhttplive";
/// The People Chain signing prefix `candidateSignature` covers (protocol
/// constant, pinned by the register wire fixtures too).
const POP_REGISTER_PREFIX: &[u8] = b"pop:people-lite:register using";

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

/// A genuinely-signed registration body for `base` (the `//Alice` candidate).
fn claim_body(base: &str, voucher: Option<&str>) -> serde_json::Value {
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

    let mut body = serde_json::json!({
        "candidateAccountId": candidate.to_account_id().to_string(),
        "username": base,
        "candidateSignature": format!("0x{}", hex::encode(signature)),
        "ringVrfKey": format!("0x{}", hex::encode(ring)),
        "proofOfOwnership": format!("0x{}", "03".repeat(64)),
        "consumerRegistrationSignature": format!("0x{}", "04".repeat(64)),
        "identifierKey": format!("0x{}", "05".repeat(65))
    });
    if let Some(key) = voucher {
        body["lifetimePoUDVoucher"] = serde_json::json!(key);
    }
    body
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

/// A lowercase-letters-only unique base name (the register pattern forbids
/// digits), derived from the pid.
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
async fn voucher_claim_is_instant_over_the_production_router() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let rpc_url = std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
    let chain = ChainClient::connect(&rpc_url).await.expect("live RPC");

    let mut config = Config::test_default();
    // Queue ON to prove the voucher lane bypasses it; vouchers ON to
    // recognise the field.
    config.queue_enabled = true;
    config.registration_vouchers_enabled = true;
    let jwt = Jwt::new(&JWT_SEED, config.jwt_issuer.clone());
    let app = identity_service::routes(AppState::new(pool.clone(), chain, jwt, config));
    let token = mint_token();

    let base = unique_base("vhttp");
    let batch = base.clone();
    let key = format!("http-live-key-{}", std::process::id());
    sqlx::query(
        "INSERT INTO registration_vouchers (key_hash, minted_batch, expires_at) \
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(eligibility::key_hash(&key))
    .bind(&batch)
    .execute(&pool)
    .await
    .expect("mint voucher");

    // Claim with the voucher: 202 INSTANT, no queue fields, RESERVED row.
    let (status, body) = post_claim(&app, &token, &claim_body(&base, Some(&key))).await;
    assert_eq!(status, 202, "voucher claim: {body}");
    assert_eq!(body["registrationOutcome"], "INSTANT", "{body}");
    assert_eq!(body["base_username"], base.as_str(), "{body}");
    assert!(
        body.get("queue").is_none(),
        "INSTANT must skip the queue: {body}"
    );
    let row = sqlx::query(
        "SELECT status, queue_group FROM username_reservations WHERE full_username = $1",
    )
    .bind(body["username"].as_str().expect("username"))
    .fetch_one(&pool)
    .await
    .expect("reservation row");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "RESERVED");
    assert_eq!(
        eligibility::voucher_state(&pool, &key).await.unwrap(),
        eligibility::VoucherState::Spent
    );

    // Reusing the voucher rejects the claim outright.
    let (status, body) = post_claim(&app, &token, &claim_body(&base, Some(&key))).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "Voucher already used", "{body}");

    // An unknown voucher rejects too — it never falls through to a free lane.
    let (status, body) = post_claim(&app, &token, &claim_body(&base, Some("no-such-key"))).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "Voucher not found", "{body}");

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
