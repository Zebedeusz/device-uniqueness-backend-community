//! Live-Postgres coverage for the reservation-outbox state machine and the
//! writer lease that guards it.
//!
//! The single-writer safety property lives here: a stale writer — expired
//! lease, superseded epoch, or replaced holder — must never advance a row.
//! Every test uses its own unique base/lease names, so the suite is safe to
//! run in parallel and against a shared database.

use std::time::Duration;

use identity_service::chain::lease;
use identity_service::chain::outbox::{self, Guard, InsertError, NewReservation, Status};

async fn test_pool() -> sqlx::PgPool {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate")
}

fn reservation(base: &str, digits: &str) -> NewReservation {
    NewReservation {
        account_id: "outbox-live-subject".to_string(),
        candidate_account_id: "outbox-live-candidate".to_string(),
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

async fn set_status(pool: &sqlx::PgPool, full_username: &str, status: &str) {
    sqlx::query("UPDATE username_reservations SET status = $1 WHERE full_username = $2")
        .bind(status)
        .bind(full_username)
        .execute(pool)
        .await
        .expect("set status");
}

async fn status_of(pool: &sqlx::PgPool, id: i64) -> String {
    sqlx::query_scalar("SELECT status FROM username_reservations WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("status")
}

async fn cleanup(pool: &sqlx::PgPool, base: &str, lease_name: &str) {
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(base)
        .execute(pool)
        .await
        .expect("cleanup rows");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(lease_name)
        .execute(pool)
        .await
        .expect("cleanup lease");
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn duplicate_full_username_maps_to_conflict() {
    let pool = test_pool().await;
    let base = format!("outboxconflict{}", std::process::id());

    outbox::insert(&pool, &reservation(&base, "11"))
        .await
        .expect("first insert");
    let err = outbox::insert(&pool, &reservation(&base, "11"))
        .await
        .expect_err("duplicate");
    assert!(matches!(err, InsertError::Conflict), "{err}");
    // The queued intake hits the same uniqueness: a raced duplicate maps to
    // Conflict there too, not a bare Db error.
    let err = outbox::insert_queued(&pool, &reservation(&base, "11"), 1)
        .await
        .expect_err("queued duplicate");
    assert!(matches!(err, InsertError::Conflict), "{err}");

    cleanup(&pool, &base, "unused").await;
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn claim_due_picks_reserved_and_gate_passed_retries_oldest_first() {
    let pool = test_pool().await;
    let base = format!("outboxdue{}", std::process::id());

    // Insert in a known order; a distinct created_at per row makes the
    // oldest-first assertion deterministic.
    for digits in ["11", "12", "13", "14", "15"] {
        outbox::insert(&pool, &reservation(&base, digits))
            .await
            .expect("insert");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 11 stays RESERVED. 12: retry gate passed. 13: retry gate in the future.
    // 14: retry with no gate at all. 15: in flight.
    sqlx::query(
        "UPDATE username_reservations SET status='RETRY_AFTER', not_before = now() - interval '1 second' \
         WHERE full_username = $1",
    )
    .bind(format!("{base}.12"))
    .execute(&pool)
    .await
    .expect("gate 12");
    sqlx::query(
        "UPDATE username_reservations SET status='RETRY_AFTER', not_before = now() + interval '1 hour' \
         WHERE full_username = $1",
    )
    .bind(format!("{base}.13"))
    .execute(&pool)
    .await
    .expect("gate 13");
    sqlx::query(
        "UPDATE username_reservations SET status='RETRY_AFTER', not_before = NULL \
         WHERE full_username = $1",
    )
    .bind(format!("{base}.14"))
    .execute(&pool)
    .await
    .expect("gate 14");
    set_status(&pool, &format!("{base}.15"), "SUBMITTING").await;

    // The table is shared with other suites, so assert on our base only.
    let due: Vec<String> = outbox::claim_due(&pool, 10_000)
        .await
        .expect("claim due")
        .into_iter()
        .map(|r| r.full_username)
        .filter(|u| u.starts_with(&base))
        .collect();
    assert_eq!(
        due,
        vec![
            format!("{base}.11"),
            format!("{base}.12"),
            format!("{base}.14")
        ],
        "RESERVED + gate-passed/ungated RETRY_AFTER, oldest first; \
         future-gated and SUBMITTING excluded"
    );

    // The startup reconciliation sweep sees exactly the in-flight row.
    let submitting: Vec<String> = outbox::submitting(&pool)
        .await
        .expect("submitting")
        .into_iter()
        .map(|r| r.full_username)
        .filter(|u| u.starts_with(&base))
        .collect();
    assert_eq!(submitting, vec![format!("{base}.15")]);

    cleanup(&pool, &base, "unused").await;
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn stale_guards_never_advance_a_row() {
    let pool = test_pool().await;
    let pid = std::process::id();
    let base = format!("outboxguard{pid}");
    let lease_name = format!("outbox-live-writer-{pid}");

    let epoch = lease::try_acquire(&pool, &lease_name, "holder-a", Duration::from_secs(60))
        .await
        .expect("acquire")
        .expect("lease free");
    let guard = Guard {
        lease_name: lease_name.clone(),
        holder_id: "holder-a".to_string(),
        epoch,
    };

    let id = outbox::insert(&pool, &reservation(&base, "21"))
        .await
        .expect("insert");

    // The held lease advances the row through the happy path.
    assert!(
        outbox::mark_submitting(&pool, &guard, id, "0xabc", 7, 1)
            .await
            .expect("mark submitting"),
        "held lease must advance RESERVED -> SUBMITTING"
    );
    assert_eq!(status_of(&pool, id).await, "SUBMITTING");

    // A superseded epoch is stale even while the lease row is alive.
    let wrong_epoch = Guard {
        epoch: epoch + 1,
        ..guard.clone()
    };
    assert!(
        !outbox::mark_assigned(&pool, &wrong_epoch, id)
            .await
            .expect("mark assigned"),
        "wrong epoch must not advance"
    );
    // A different holder id is stale even with the right epoch.
    let wrong_holder = Guard {
        holder_id: "holder-b".to_string(),
        ..guard.clone()
    };
    assert!(
        !outbox::mark_failed(&pool, &wrong_holder, id, "nope")
            .await
            .expect("mark failed"),
        "foreign holder must not advance"
    );
    assert_eq!(status_of(&pool, id).await, "SUBMITTING");

    // Expire the lease: the old guard is now dead for every mutation.
    sqlx::query("UPDATE writer_lease SET expires_at = now() - interval '1 second' WHERE name = $1")
        .bind(&lease_name)
        .execute(&pool)
        .await
        .expect("expire lease");
    let not_before = time::OffsetDateTime::now_utc() + time::Duration::seconds(60);
    assert!(
        !outbox::mark_retry(&pool, &guard, id, not_before, 2, "transient")
            .await
            .expect("mark retry"),
        "expired lease must not advance"
    );
    assert_eq!(status_of(&pool, id).await, "SUBMITTING");

    // A new holder takes over at a bumped epoch; only ITS guard works now.
    let new_epoch = lease::try_acquire(&pool, &lease_name, "holder-b", Duration::from_secs(60))
        .await
        .expect("takeover")
        .expect("expired lease is takeable");
    assert!(new_epoch > epoch, "takeover must bump the epoch");
    assert!(
        !outbox::mark_assigned(&pool, &guard, id)
            .await
            .expect("mark assigned"),
        "the replaced writer's guard stays dead after takeover"
    );
    let new_guard = Guard {
        lease_name: lease_name.clone(),
        holder_id: "holder-b".to_string(),
        epoch: new_epoch,
    };
    assert!(
        outbox::mark_retry(&pool, &new_guard, id, not_before, 2, "transient")
            .await
            .expect("mark retry"),
        "the live holder's guard advances"
    );
    assert_eq!(status_of(&pool, id).await, "RETRY_AFTER");
    let (attempt, last_error): (i32, Option<String>) =
        sqlx::query_as("SELECT attempt, last_error FROM username_reservations WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(attempt, 2);
    assert_eq!(last_error.as_deref(), Some("transient"));

    assert!(outbox::mark_assigned(&pool, &new_guard, id)
        .await
        .expect("mark assigned"));
    assert_eq!(status_of(&pool, id).await, "ASSIGNED");

    cleanup(&pool, &base, &lease_name).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn lease_lifecycle_acquire_renew_expire_takeover() {
    let pool = test_pool().await;
    let name = format!("outbox-live-lease-{}", std::process::id());
    let ttl = Duration::from_secs(60);

    // Fresh lease starts at epoch 1; re-acquiring our own bumps the epoch.
    let first = lease::try_acquire(&pool, &name, "holder-a", ttl)
        .await
        .expect("acquire")
        .expect("free lease");
    let second = lease::try_acquire(&pool, &name, "holder-a", ttl)
        .await
        .expect("acquire")
        .expect("own lease is retakeable");
    assert!(second > first);

    // A live lease blocks other holders and shows alive.
    assert!(lease::try_acquire(&pool, &name, "holder-b", ttl)
        .await
        .expect("acquire")
        .is_none());
    assert!(lease::alive(&pool, &name).await.expect("alive"));

    // Renewal needs the exact holder + epoch.
    assert!(lease::renew(&pool, &name, "holder-a", second, ttl)
        .await
        .expect("renew"));
    assert!(!lease::renew(&pool, &name, "holder-a", first, ttl)
        .await
        .expect("renew"));
    assert!(!lease::renew(&pool, &name, "holder-b", second, ttl)
        .await
        .expect("renew"));

    // An expired lease must not be silently resurrected by renew — the holder
    // has to go back through try_acquire (new epoch), because liveness readers
    // may already have observed it dead.
    sqlx::query("UPDATE writer_lease SET expires_at = now() - interval '1 second' WHERE name = $1")
        .bind(&name)
        .execute(&pool)
        .await
        .expect("expire");
    assert!(!lease::alive(&pool, &name).await.expect("alive"));
    assert!(!lease::renew(&pool, &name, "holder-a", second, ttl)
        .await
        .expect("renew"));
    let taken = lease::try_acquire(&pool, &name, "holder-b", ttl)
        .await
        .expect("acquire")
        .expect("expired lease is takeable");
    assert!(taken > second);

    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(&name)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn depth_by_status_reports_every_status_even_when_empty() {
    let pool = test_pool().await;
    let base = format!("outboxdepth{}", std::process::id());

    outbox::insert(&pool, &reservation(&base, "31"))
        .await
        .expect("insert");

    let depths = outbox::depth_by_status(&pool).await.expect("depths");
    // The zero-fill contract: all six statuses, in Status::ALL order, always —
    // a gauge that vanishes when a state empties is unalertable.
    assert_eq!(
        depths.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        Status::ALL.to_vec()
    );
    let reserved = depths
        .iter()
        .find(|(s, _)| *s == Status::Reserved)
        .expect("reserved entry");
    assert!(reserved.1.depth >= 1);
    assert!(reserved.1.oldest_age_secs.is_some());
    for (_, depth) in &depths {
        assert!(depth.depth >= 0);
        if depth.depth == 0 {
            assert!(depth.oldest_age_secs.is_none());
        }
    }

    cleanup(&pool, &base, "unused").await;
}

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn oldest_age_measures_time_in_status_not_row_age() {
    let pool = test_pool().await;
    let base = format!("outboxage{}", std::process::id());
    let id = outbox::insert(&pool, &reservation(&base, "32"))
        .await
        .expect("insert");

    // A row created two hours ago that entered its current status a moment ago.
    // That is every retried or just-promoted claim: measured from `created_at`
    // it reads as two hours stuck, which is what made this gauge unalertable.
    sqlx::query(
        "UPDATE username_reservations SET created_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("backdate");

    let depths = outbox::depth_by_status(&pool).await.expect("depths");
    let (_, reserved) = depths
        .iter()
        .find(|(s, _)| *s == Status::Reserved)
        .expect("reserved entry");
    let age = reserved
        .oldest_age_secs
        .expect("non-empty status has an age");
    assert!(
        age < 600.0,
        "age must come from updated_at, not created_at; got {age}s"
    );

    cleanup(&pool, &base, "unused").await;
}
