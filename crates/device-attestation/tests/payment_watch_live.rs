//! Live coverage (Postgres + live People Chain RPC) for the payment watcher:
//! expiry, non-detection on an unfunded address, and the confirmation
//! hand-off — PENDING → CONFIRMED + a `RESERVED` outbox row in one
//! transaction, with the discriminator re-selected at confirmation time.

use std::time::Duration;

use device_attestation::chain::outbox::{self, NewReservation};
use device_attestation::config::PaymentConfig;
use device_attestation::payment::{self, ClaimPayload, ConfirmOutcome};
use device_attestation::ChainClient;
use sqlx::Row as _;

fn reservation(base: &str, digits: &str, subject: &str) -> NewReservation {
    NewReservation {
        account_id: subject.to_string(),
        candidate_account_id: "watch-test-candidate".to_string(),
        base: base.to_string(),
        digits: digits.to_string(),
        full_username: format!("{base}.{digits}"),
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
    let database_url = std::env::var("DEVICE_ATTESTATION_TEST_DATABASE_URL")
        .expect("DEVICE_ATTESTATION_TEST_DATABASE_URL is required");
    device_attestation::db::connect(&database_url)
        .await
        .expect("connect and migrate")
}

async fn chain() -> ChainClient {
    let rpc_url = std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://paseo-people-next-system-rpc.polkadot.io".to_string());
    ChainClient::connect(&rpc_url).await.expect("live RPC")
}

/// The subject's payment-request id (most recent).
async fn request_id(pool: &sqlx::PgPool, subject: &str) -> i64 {
    sqlx::query("SELECT id FROM payment_requests WHERE account_id = $1 ORDER BY id DESC")
        .bind(subject)
        .fetch_one(pool)
        .await
        .expect("request row")
        .try_get("id")
        .unwrap()
}

async fn request_status(pool: &sqlx::PgPool, id: i64) -> String {
    sqlx::query("SELECT status FROM payment_requests WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("request row")
        .try_get("status")
        .unwrap()
}

async fn cleanup(pool: &sqlx::PgPool, subject: &str, base: &str) {
    sqlx::query("DELETE FROM payment_requests WHERE account_id = $1")
        .bind(subject)
        .execute(pool)
        .await
        .expect("clean payment requests");
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(base)
        .execute(pool)
        .await
        .expect("clean reservations");
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
#[ignore = "requires Postgres (DEVICE_ATTESTATION_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn confirmation_hands_off_to_the_outbox_exactly_once() {
    let pool = connect().await;
    let chain = chain().await;
    let base = unique_base("watchok");
    let subject = format!("watch-confirm-{}", std::process::id());
    cleanup(&pool, &subject, &base).await;

    let intent = reservation(&base, "01", &subject);
    payment::quote(
        &pool,
        &config(),
        &subject,
        &ClaimPayload::from_reservation(&intent, Some("07")),
    )
    .await
    .expect("quote");
    let id = request_id(&pool, &subject).await;

    // Confirm (support-path entry, bypassing the balance check): the request
    // flips CONFIRMED and the reservation lands RESERVED with the preferred
    // digits honored (re-selected at confirmation, both free on this base).
    let outcome = payment::confirm_by_id(&pool, &chain, id)
        .await
        .expect("confirm");
    let reservation_id = match outcome {
        Some(ConfirmOutcome::Confirmed(reservation_id)) => reservation_id,
        other => panic!("expected Confirmed, got {other:?}"),
    };
    assert_eq!(request_status(&pool, id).await, "CONFIRMED");
    let row = sqlx::query("SELECT status, full_username FROM username_reservations WHERE id = $1")
        .bind(reservation_id)
        .fetch_one(&pool)
        .await
        .expect("reservation row");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "RESERVED");
    assert_eq!(
        row.try_get::<String, _>("full_username").unwrap(),
        format!("{base}.07"),
        "preferred digits honored at confirmation"
    );
    let confirmed_at: Option<time::OffsetDateTime> =
        sqlx::query("SELECT confirmed_at FROM payment_requests WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("row")
            .try_get("confirmed_at")
            .unwrap();
    assert!(confirmed_at.is_some());

    // Idempotence: the row is no longer PENDING, so a second confirmation
    // attempt finds nothing to do.
    assert_eq!(
        payment::confirm_by_id(&pool, &chain, id)
            .await
            .expect("second confirm"),
        None
    );

    cleanup(&pool, &subject, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres (DEVICE_ATTESTATION_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn taken_preferred_digits_substitute_a_random_free_discriminator() {
    let pool = connect().await;
    let chain = chain().await;
    let base = unique_base("watchsub");
    let subject = format!("watch-substitute-{}", std::process::id());
    cleanup(&pool, &subject, &base).await;

    // The preferred discriminator is taken by the time the deposit lands.
    outbox::insert(&pool, &reservation(&base, "07", "someone-else"))
        .await
        .expect("pre-take the preferred digit");
    let intent = reservation(&base, "01", &subject);
    payment::quote(
        &pool,
        &config(),
        &subject,
        &ClaimPayload::from_reservation(&intent, Some("07")),
    )
    .await
    .expect("quote");
    let id = request_id(&pool, &subject).await;

    // A paid registration never fails for digit contention: confirmation
    // re-selects a random free discriminator instead of honoring the taken
    // preference (the divergence from the claim path's 409).
    let outcome = payment::confirm_by_id(&pool, &chain, id)
        .await
        .expect("confirm");
    let reservation_id = match outcome {
        Some(ConfirmOutcome::Confirmed(reservation_id)) => reservation_id,
        other => panic!("expected Confirmed, got {other:?}"),
    };
    assert_eq!(request_status(&pool, id).await, "CONFIRMED");
    let row = sqlx::query("SELECT status, digits FROM username_reservations WHERE id = $1")
        .bind(reservation_id)
        .fetch_one(&pool)
        .await
        .expect("reservation row");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "RESERVED");
    let digits: String = row.try_get("digits").unwrap();
    assert_ne!(digits, "07", "taken preference must not be double-assigned");
    let digit: u8 = digits.parse().expect("two-digit discriminator");
    assert!((1..=99).contains(&digit));

    cleanup(&pool, &subject, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres (DEVICE_ATTESTATION_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn an_unparseable_address_row_does_not_poison_the_pass() {
    let pool = connect().await;
    let chain = chain().await;
    let base = unique_base("watchpoi");
    let poison_subject = format!("watch-poison-{}", std::process::id());
    let healthy_subject = format!("watch-healthy-{}", std::process::id());
    cleanup(&pool, &poison_subject, &base).await;
    cleanup(&pool, &healthy_subject, &base).await;

    // An (impossible-by-construction, but total-outage-if-it-happens)
    // corrupted address on the OLDER row: the pass walks rows in created_at
    // order, so erroring on it would freeze every request created after it.
    let intent = reservation(&base, "01", &poison_subject);
    payment::quote(
        &pool,
        &config(),
        &poison_subject,
        &ClaimPayload::from_reservation(&intent, None),
    )
    .await
    .expect("poison quote");
    let poison_id = request_id(&pool, &poison_subject).await;
    sqlx::query("UPDATE payment_requests SET payment_address = 'not-an-address' WHERE id = $1")
        .bind(poison_id)
        .execute(&pool)
        .await
        .expect("corrupt address");

    let intent = reservation(&base, "01", &healthy_subject);
    payment::quote(
        &pool,
        &config(),
        &healthy_subject,
        &ClaimPayload::from_reservation(&intent, None),
    )
    .await
    .expect("healthy quote");
    let healthy_id = request_id(&pool, &healthy_subject).await;

    // The pass must skip the poison row and still check the healthy one —
    // not error out. Both stay PENDING (neither address is funded).
    payment::watch_pass(&pool, &chain)
        .await
        .expect("watch pass survives the poison row");
    assert_eq!(request_status(&pool, poison_id).await, "PENDING");
    assert_eq!(request_status(&pool, healthy_id).await, "PENDING");

    cleanup(&pool, &poison_subject, &base).await;
    cleanup(&pool, &healthy_subject, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres (DEVICE_ATTESTATION_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn unfunded_quote_stays_pending_through_a_watch_pass() {
    let pool = connect().await;
    let chain = chain().await;
    let base = unique_base("watchpend");
    let subject = format!("watch-pending-{}", std::process::id());
    cleanup(&pool, &subject, &base).await;

    let intent = reservation(&base, "01", &subject);
    payment::quote(
        &pool,
        &config(),
        &subject,
        &ClaimPayload::from_reservation(&intent, None),
    )
    .await
    .expect("quote");
    let id = request_id(&pool, &subject).await;

    // The deposit multisig of the throwaway master holds no funds on the
    // live chain: detection must NOT confirm, and must not error the pass.
    payment::watch_pass(&pool, &chain)
        .await
        .expect("watch pass");
    assert_eq!(request_status(&pool, id).await, "PENDING");

    cleanup(&pool, &subject, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres (DEVICE_ATTESTATION_TEST_DATABASE_URL) and a reachable People Chain RPC \
            (PEOPLE_RPC_URL or the Paseo default); run with --ignored"]
async fn paid_request_with_an_exhausted_base_fails_conflict() {
    let pool = connect().await;
    let chain = chain().await;
    let base = unique_base("watchfull");
    let subject = format!("watch-full-{}", std::process::id());
    cleanup(&pool, &subject, &base).await;

    // Exhaust the base: every discriminator 01..=99 already in the outbox.
    for digit in 1..=99u8 {
        let digits = format!("{digit:02}");
        outbox::insert(&pool, &reservation(&base, &digits, "someone-else"))
            .await
            .expect("pre-fill digit");
    }
    let intent = reservation(&base, "01", &subject);
    payment::quote(
        &pool,
        &config(),
        &subject,
        &ClaimPayload::from_reservation(&intent, None),
    )
    .await
    .expect("quote");
    let id = request_id(&pool, &subject).await;

    let outcome = payment::confirm_by_id(&pool, &chain, id)
        .await
        .expect("confirm attempt");
    assert_eq!(outcome, Some(ConfirmOutcome::Exhausted));
    assert_eq!(
        request_status(&pool, id).await,
        "FAILED_CONFLICT",
        "money observed but unregistrable — kept for support"
    );

    cleanup(&pool, &subject, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn stale_pending_quotes_expire() {
    let pool = connect().await;
    let base = unique_base("watchexp");
    let subject = format!("watch-expire-{}", std::process::id());
    cleanup(&pool, &subject, &base).await;

    let intent = reservation(&base, "01", &subject);
    payment::quote(
        &pool,
        &config(),
        &subject,
        &ClaimPayload::from_reservation(&intent, None),
    )
    .await
    .expect("quote");
    let id = request_id(&pool, &subject).await;
    sqlx::query(
        "UPDATE payment_requests SET expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("backdate");

    let expired = payment::expire_pending(&pool).await.expect("expire pass");
    assert!(expired >= 1);
    assert_eq!(request_status(&pool, id).await, "EXPIRED");

    cleanup(&pool, &subject, &base).await;
}
