//! End-to-end auth-handshake smoke: challenge -> token -> refresh.
//!
//! Drives a running identity-service the way the shipping clients do — issues an
//! sr25519 proof over `SHA256(challenge || clientId || SHA256(body))` with a
//! `//Alice` keypair (a real Substrate signer), then decodes the returned JWT to
//! show the `accountId` claim. Seeds the Slice 5 golden/client smoke.
//!
//! Usage: `cargo run -p identity-service --example auth_smoke -- [base_url]`

use std::str::FromStr as _;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use subxt::utils::AccountId32;
use subxt_signer::sr25519::Keypair;
use subxt_signer::SecretUri;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string());
    let http = reqwest::Client::new();

    // 1. challenge
    let challenge_b64 = http
        .post(format!("{base}/api/v1/auth/challenges"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?["challenge"]
        .as_str()
        .expect("challenge string")
        .to_string();
    println!("1. challenge: {challenge_b64}");

    // 2. build the sr25519 proof over SHA256(challenge || clientId || SHA256(body))
    let keypair = Keypair::from_uri(&SecretUri::from_str("//Alice")?)?;
    let client_id = keypair.public_key().0;
    let challenge = STANDARD.decode(&challenge_b64)?;
    let body = b"{}";
    let mut hasher = Sha256::new();
    hasher.update(&challenge);
    hasher.update(client_id);
    hasher.update(Sha256::digest(body));
    let message: [u8; 32] = hasher.finalize().into();
    let proof = keypair.sign(&message).0;

    // 3. token
    let token_res = http
        .post(format!("{base}/api/v1/auth/token"))
        .header("Auth-ClientId", STANDARD.encode(client_id))
        .header("Auth-ClientProof", STANDARD.encode(proof))
        .header("Auth-Challenge", &challenge_b64)
        .header("Auth-iOS-Package", "io.pcf.polkadotapp")
        .body(body.to_vec())
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let token = token_res["token"].as_str().expect("token").to_string();
    let refresh_token = token_res["refreshToken"]
        .as_str()
        .expect("refreshToken")
        .to_string();
    println!("2. token: {}...", &token[..48.min(token.len())]);

    let claims: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD.decode(token.split('.').nth(1).expect("jwt payload"))?,
    )?;
    println!("   JWT accountId claim: {}", claims["accountId"]);
    println!(
        "   JWT appFromOfficialStore: {}",
        claims["appFromOfficialStore"]
    );
    assert_eq!(
        claims["accountId"].as_str(),
        Some(format!("0x{}", hex::encode(client_id)).as_str())
    );

    // 4. refresh (rotate)
    let refreshed = http
        .post(format!("{base}/api/v1/auth/token/refresh"))
        .json(&serde_json::json!({ "refreshToken": refresh_token }))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    println!(
        "3. refreshed token: {}...",
        &refreshed["token"].as_str().expect("token")[..48]
    );

    // 5. attester public key (public route)
    let attester = http
        .get(format!("{base}/api/v1/attester"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    println!("4. attester: {}", attester["attester"]);

    // 6. availability (JWT-gated) against a known-taken base + a free base + an invalid one
    let available = http
        .post(format!("{base}/api/v1/usernames/available?version=v1"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "usernames": ["tqegciilc", "zzzxqwvb", "abc"] }))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    let value = &available["value"];
    println!(
        "5. available tqegciilc: status={} digits_len={} (1 excluded? {})",
        value["tqegciilc"]["status"],
        value["tqegciilc"]["availableDigits"]
            .as_array()
            .map_or(0, |a| a.len()),
        !value["tqegciilc"]["availableDigits"]
            .as_array()
            .map(|a| a.iter().any(|d| d.as_u64() == Some(1)))
            .unwrap_or(true)
    );
    println!(
        "   available zzzxqwvb: status={} digits_len={}",
        value["zzzxqwvb"]["status"],
        value["zzzxqwvb"]["availableDigits"]
            .as_array()
            .map_or(0, |a| a.len())
    );
    println!(
        "   available abc (too short): status={}",
        value["abc"]["status"]
    );
    assert_eq!(value["abc"]["status"], "INVALID");
    assert_eq!(value["tqegciilc"]["status"], "AVAILABLE");

    // 7. register (write path) — reserves a name and returns 202. The backend
    //    validates shape + `who` == JWT subject; the attestation payload is
    //    relayed to chain, so dummy fixed-length hex is fine for the intake demo.
    //    `candidateAccountId` is Alice's SS58 (== the JWT subject).
    let candidate_ss58 = AccountId32(client_id).to_string();
    let register_body = serde_json::json!({
        "candidateAccountId": candidate_ss58,
        "username": "smokezalice",
        "candidateSignature": format!("0x{}", "00".repeat(64)),
        "ringVrfKey": format!("0x{}", "00".repeat(32)),
        "proofOfOwnership": format!("0x{}", "00".repeat(64)),
        "consumerRegistrationSignature": format!("0x{}", "00".repeat(64)),
        "identifierKey": format!("0x{}", "00".repeat(65)),
    });
    let register = http
        .post(format!("{base}/api/v1/usernames"))
        .bearer_auth(&token)
        .json(&register_body)
        .send()
        .await?;
    let register_status = register.status();
    let register_json = register.json::<serde_json::Value>().await?;
    println!("6. register status={register_status} body={register_json}");
    // 202 (reserved) or 409 (digit already taken on a repeat run) both prove intake.
    assert!(
        register_status.as_u16() == 202 || register_status.as_u16() == 409,
        "register intake should return 202 or 409, got {register_status}"
    );
    if register_status.as_u16() == 202 {
        assert_eq!(register_json["base_username"], "smokezalice");
        assert!(register_json["username"]
            .as_str()
            .is_some_and(|u| u.starts_with("smokezalice.")));
    }

    // 8. who != subject is rejected (403): register for Bob while holding Alice's JWT.
    let bob = Keypair::from_uri(&SecretUri::from_str("//Bob")?)?;
    let bob_ss58 = AccountId32(bob.public_key().0).to_string();
    let mismatch_body = {
        let mut b = register_body.clone();
        b["candidateAccountId"] = serde_json::json!(bob_ss58);
        b["username"] = serde_json::json!("smokezbobxx");
        b
    };
    let mismatch = http
        .post(format!("{base}/api/v1/usernames"))
        .bearer_auth(&token)
        .json(&mismatch_body)
        .send()
        .await?;
    println!(
        "7. who != subject status={} (expect 403)",
        mismatch.status()
    );
    assert_eq!(mismatch.status().as_u16(), 403);

    println!(
        "\nOK: challenge -> token -> refresh -> attester -> available -> register (who-bound) all succeeded"
    );
    Ok(())
}
