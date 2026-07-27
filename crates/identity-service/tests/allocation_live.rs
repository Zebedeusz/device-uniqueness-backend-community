//! Live-Postgres coverage for reservation-outbox allocation visibility.

use std::collections::BTreeSet;

use identity_service::chain::outbox::{self, NewReservation, Status};

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn every_outbox_status_remains_allocated() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let base = format!("allocationtest{}", std::process::id());

    for (digits, status) in [
        ("11", Status::Reserved),
        ("12", Status::Submitting),
        ("13", Status::Assigned),
        ("14", Status::RetryAfter),
        ("15", Status::FailedTerminal),
    ] {
        let full_username = format!("{base}.{digits}");
        let reservation = NewReservation {
            account_id: "test-subject".to_string(),
            candidate_account_id: "test-candidate".to_string(),
            base: base.clone(),
            digits: digits.to_string(),
            full_username: full_username.clone(),
            candidate_signature: vec![1; 64],
            ring_vrf_key: vec![2; 32],
            proof_of_ownership: vec![3; 64],
            consumer_registration_signature: vec![4; 64],
            identifier_key: vec![5; 65],
            dotns_signature: None,
            dotns_signed_at: None,
            reserved_username: None,
        };
        outbox::insert(&pool, &reservation).await.expect("insert");
        sqlx::query("UPDATE username_reservations SET status = $1 WHERE full_username = $2")
            .bind(status.as_str())
            .bind(&full_username)
            .execute(&pool)
            .await
            .expect("set status");
    }

    let allocated = outbox::allocated_discriminators(&pool, &base)
        .await
        .expect("read allocations");
    assert_eq!(allocated, BTreeSet::from([11, 12, 13, 14, 15]));

    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean test rows");
}
