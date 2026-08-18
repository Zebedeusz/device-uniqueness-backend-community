//! Live-Postgres coverage for the registration-voucher INSTANT lane:
//! state resolution, the atomic redeem + reserve transaction, single-use
//! under concurrency, and the conflict-preserves-voucher invariant.

use device_attestation::chain::outbox::NewReservation;
use device_attestation::eligibility::{
    self, redeem_and_reserve, voucher_state, RedeemError, VoucherError, VoucherState,
};
use sqlx::Row as _;

fn reservation(base: &str, digits: &str) -> NewReservation {
    NewReservation {
        account_id: "voucher-test-account".to_string(),
        candidate_account_id: "voucher-test-candidate".to_string(),
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

async fn connect() -> sqlx::PgPool {
    let database_url = std::env::var("DEVICE_ATTESTATION_TEST_DATABASE_URL")
        .expect("DEVICE_ATTESTATION_TEST_DATABASE_URL is required");
    device_attestation::db::connect(&database_url)
        .await
        .expect("connect and migrate")
}

/// Insert a voucher exactly as the mint CLI does (hash only), with the given
/// validity offset in seconds relative to now. The batch label is the test's
/// `base` so concurrent tests never clean up each other's vouchers.
async fn mint(pool: &sqlx::PgPool, batch: &str, key: &str, expires_in_secs: i64) {
    sqlx::query(
        "INSERT INTO registration_vouchers (key_hash, minted_batch, expires_at) \
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(eligibility::key_hash(key))
    .bind(batch)
    .bind(expires_in_secs as f64)
    .execute(pool)
    .await
    .expect("mint voucher");
}

async fn cleanup(pool: &sqlx::PgPool, base: &str) {
    sqlx::query("DELETE FROM registration_vouchers WHERE minted_batch = $1")
        .bind(base)
        .execute(pool)
        .await
        .expect("clean vouchers");
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(base)
        .execute(pool)
        .await
        .expect("clean reservations");
}

#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn redeem_reserves_burns_once_and_preserves_the_voucher_on_conflict() {
    let pool = connect().await;
    let base = format!("vouchertest{}", std::process::id());
    cleanup(&pool, &base).await;

    let key = format!("live-key-{}", std::process::id());
    mint(&pool, &base, &key, 3600).await;
    assert_eq!(
        voucher_state(&pool, &key).await.unwrap(),
        VoucherState::Redeemable
    );
    assert_eq!(
        voucher_state(&pool, "no-such-key").await.unwrap(),
        VoucherState::Unknown
    );

    // Redeem: reservation lands as RESERVED (skips the queue lane by
    // construction) and the voucher flips to Spent.
    let id = redeem_and_reserve(&pool, &key, &reservation(&base, "01"))
        .await
        .expect("first redeem succeeds");
    let status: String = sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("row exists")
        .try_get("status")
        .unwrap();
    assert_eq!(status, "RESERVED");
    assert_eq!(
        voucher_state(&pool, &key).await.unwrap(),
        VoucherState::Spent
    );

    // Single-use: the second redeem of the same key fails and inserts nothing.
    match redeem_and_reserve(&pool, &key, &reservation(&base, "02")).await {
        Err(RedeemError::Voucher(VoucherError::Spent)) => {}
        other => panic!("expected Spent, got {other:?}"),
    }

    // Conflict preserves the voucher: a fresh voucher against an
    // already-taken username must roll the burn back.
    let key2 = format!("live-key2-{}", std::process::id());
    mint(&pool, &base, &key2, 3600).await;
    match redeem_and_reserve(&pool, &key2, &reservation(&base, "01")).await {
        Err(RedeemError::Conflict) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        voucher_state(&pool, &key2).await.unwrap(),
        VoucherState::Redeemable,
        "a username conflict must not consume the voucher"
    );

    // Expired voucher: state reports Expired and the redeem refuses.
    let key3 = format!("live-key3-{}", std::process::id());
    mint(&pool, &base, &key3, -60).await;
    assert_eq!(
        voucher_state(&pool, &key3).await.unwrap(),
        VoucherState::Expired
    );
    match redeem_and_reserve(&pool, &key3, &reservation(&base, "03")).await {
        Err(RedeemError::Voucher(VoucherError::Expired)) => {}
        other => panic!("expected Expired, got {other:?}"),
    }

    cleanup(&pool, &base).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn concurrent_redeems_of_one_voucher_admit_exactly_one() {
    let pool = connect().await;
    let base = format!("voucherrace{}", std::process::id());
    cleanup(&pool, &base).await;

    let key = format!("race-key-{}", std::process::id());
    mint(&pool, &base, &key, 3600).await;

    // Two claimants race the same voucher toward different usernames; the
    // conditional burn must admit exactly one.
    let first = reservation(&base, "11");
    let second = reservation(&base, "12");
    let (a, b) = tokio::join!(
        redeem_and_reserve(&pool, &key, &first),
        redeem_and_reserve(&pool, &key, &second),
    );
    let winners = [&a, &b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one redeem must win: {a:?} / {b:?}");
    let loser = if a.is_ok() { b } else { a };
    match loser {
        Err(RedeemError::Voucher(VoucherError::Spent)) => {}
        other => panic!("loser must see Spent, got {other:?}"),
    }

    let reserved: i64 =
        sqlx::query("SELECT count(*) AS n FROM username_reservations WHERE base = $1")
            .bind(&base)
            .fetch_one(&pool)
            .await
            .expect("count")
            .try_get("n")
            .unwrap();
    assert_eq!(reserved, 1, "exactly one reservation from one voucher");

    cleanup(&pool, &base).await;
}
