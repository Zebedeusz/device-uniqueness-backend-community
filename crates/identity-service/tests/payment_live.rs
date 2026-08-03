//! Live-Postgres coverage for the payment-quote lane: durable PENDING
//! requests, idempotent re-quote (same address, frozen amount, refreshed
//! TTL, last-intent-wins payload), and per-subject uniqueness.

use std::time::Duration;

use identity_service::chain::outbox::NewReservation;
use identity_service::config::PaymentConfig;
use identity_service::payment::{self, ClaimPayload};
use sqlx::Row as _;

fn reservation(base: &str) -> NewReservation {
    NewReservation {
        account_id: "payment-test-subject".to_string(),
        candidate_account_id: "payment-test-candidate".to_string(),
        base: base.to_string(),
        digits: "01".to_string(),
        full_username: format!("{base}.01"),
        candidate_signature: vec![1; 64],
        ring_vrf_key: vec![2; 32],
        proof_of_ownership: vec![3; 64],
        consumer_registration_signature: vec![4; 64],
        identifier_key: vec![5; 65],
        dotns_signature: None,
        dotns_signed_at: None,
        reserved_username: None,
    }
}

fn config() -> PaymentConfig {
    PaymentConfig {
        master_account: [7u8; 32],
        amount_planck: 10_000_000_000,
        request_ttl: Duration::from_secs(3600),
    }
}

async fn connect() -> sqlx::PgPool {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate")
}

async fn cleanup(pool: &sqlx::PgPool, subjects: &[&str]) {
    for subject in subjects {
        sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
            .bind(subject)
            .execute(pool)
            .await
            .expect("clean payment requests");
    }
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn quote_is_durable_idempotent_and_last_intent_wins() {
    let pool = connect().await;
    let pid = std::process::id();
    let subject = format!("payment-live-{pid}");
    let other = format!("payment-live-other-{pid}");
    cleanup(&pool, &[&subject, &other]).await;
    let config = config();

    // First claim mints a PENDING request with the frozen amount.
    let first_intent = reservation("paybasefirst");
    let quote = payment::quote(
        &pool,
        &config,
        &subject,
        &ClaimPayload::from_reservation(&first_intent, Some("07")),
    )
    .await
    .expect("first quote");
    assert_eq!(quote.amount_planck, config.amount_planck);

    let row = sqlx::query(
        "SELECT status, base, preferred_digits, payment_address, expires_at \
         FROM payment_requests WHERE account_id = $1",
    )
    .bind(&subject)
    .fetch_one(&pool)
    .await
    .expect("request row");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "PENDING");
    assert_eq!(row.try_get::<String, _>("base").unwrap(), "paybasefirst");
    assert_eq!(
        row.try_get::<Option<String>, _>("preferred_digits")
            .unwrap(),
        Some("07".to_string())
    );
    let address = row.try_get::<String, _>("payment_address").unwrap();
    assert_eq!(quote.payment_address, address);
    // The stored address is exactly the master+dummy multisig re-derived
    // from the subject alone (the sweep's derivation).
    assert_eq!(
        address,
        payment::address_ss58(&payment::deposit_account(&config.master_account, &subject))
    );
    let first_expiry: time::OffsetDateTime = row.try_get("expires_at").unwrap();

    // Re-claim with a different intent: same address, same amount, payload
    // re-targeted, TTL refreshed. No second row.
    let second_intent = reservation("paybasesecond");
    let requote = payment::quote(
        &pool,
        &config,
        &subject,
        &ClaimPayload::from_reservation(&second_intent, None),
    )
    .await
    .expect("re-quote");
    assert_eq!(requote.payment_address, quote.payment_address);
    assert_eq!(requote.amount_planck, quote.amount_planck);
    let row = sqlx::query(
        "SELECT count(*) OVER () AS n, base, preferred_digits, expires_at \
         FROM payment_requests WHERE account_id = $1",
    )
    .bind(&subject)
    .fetch_one(&pool)
    .await
    .expect("request row");
    assert_eq!(
        row.try_get::<i64, _>("n").unwrap(),
        1,
        "one row per subject"
    );
    assert_eq!(row.try_get::<String, _>("base").unwrap(), "paybasesecond");
    assert_eq!(
        row.try_get::<Option<String>, _>("preferred_digits")
            .unwrap(),
        None
    );
    assert!(
        row.try_get::<time::OffsetDateTime, _>("expires_at")
            .unwrap()
            >= first_expiry,
        "re-claim refreshes the TTL"
    );

    // A different subject gets a different deposit address.
    let other_quote = payment::quote(
        &pool,
        &config,
        &other,
        &ClaimPayload::from_reservation(&first_intent, None),
    )
    .await
    .expect("other subject quote");
    assert_ne!(other_quote.payment_address, quote.payment_address);

    cleanup(&pool, &[&subject, &other]).await;
}
