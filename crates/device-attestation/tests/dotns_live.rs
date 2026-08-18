//! Live-Postgres coverage for the dotNS gateway lane.
//!
//! Covers how a row enters the lane, what `claim_dotns_due` will and will not
//! pick up, the lease-guarded transitions, and the no-backfill property.

use device_attestation::chain::lease;
use device_attestation::chain::outbox::{self, DotnsStatus, Guard, NewReservation, Status};
use sqlx::{Connection as _, PgConnection, PgPool, Row as _};
use std::time::Duration;

const LEASE: &str = "dotns-live-test-writer";

/// Advisory lock id serialising this file's tests. Arbitrary, only has to be
/// unique among whatever else takes advisory locks on the test database.
const EXCLUSIVE_LOCK: i64 = 0x0d07_1553;

/// Takes the exclusive lock that lets a test treat `username_reservations` as
/// its own.
///
/// Both tests here assert over the **whole** table — an exact `claim_dotns_due`
/// vector and a table-wide depth total — and both clear rows with unscoped
/// DELETEs. Cargo runs them in parallel inside one binary, so unserialised they
/// race two ways: the window between test 2's insert and the UPDATE that clears
/// its `dotns_status` leaves a `PENDING` row that breaks test 1's total, and
/// test 1's blanket DELETE can remove test 2's row mid-flight.
///
/// The lock lives on its own connection rather than one borrowed from the pool,
/// because a session advisory lock is only released when its connection closes
/// — returning a pooled connection would keep holding it. Dropping the returned
/// value releases the lock, including while unwinding from a failed assertion.
async fn lock_exclusive(database_url: &str) -> PgConnection {
    let mut conn = PgConnection::connect(database_url)
        .await
        .expect("connect for advisory lock");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(EXCLUSIVE_LOCK)
        .execute(&mut conn)
        .await
        .expect("take the exclusive test lock");
    conn
}

fn reservation(base: &str, digits: &str, with_dotns: bool) -> NewReservation {
    NewReservation {
        account_id: format!("dotns-subject-{digits}"),
        candidate_account_id: "test-candidate".to_string(),
        base: base.to_string(),
        digits: digits.to_string(),
        full_username: format!("{base}.{digits}"),
        candidate_signature: vec![1; 64],
        ring_vrf_key: vec![2; 32],
        proof_of_ownership: vec![3; 64],
        consumer_registration_signature: vec![4; 64],
        identifier_key: vec![5; 65],
        dotns_signature: with_dotns.then(|| vec![6; 64]),
        dotns_signed_at: with_dotns.then_some(1_750_000_000),
        reserved_username: None,
    }
}

async fn dotns_status(pool: &PgPool, id: i64) -> Option<String> {
    sqlx::query("SELECT dotns_status FROM username_reservations WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read dotns_status")
        .try_get::<Option<String>, _>("dotns_status")
        .expect("decode dotns_status")
}

async fn people_status(pool: &PgPool, id: i64) -> String {
    sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read status")
        .try_get("status")
        .expect("decode status")
}

async fn assign_on_people(pool: &PgPool, id: i64) {
    sqlx::query("UPDATE username_reservations SET status = 'ASSIGNED' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("assign");
}

#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn the_dotns_lane_claims_only_assigned_rows_and_never_touches_people_status() {
    let database_url = std::env::var("DEVICE_ATTESTATION_TEST_DATABASE_URL")
        .expect("DEVICE_ATTESTATION_TEST_DATABASE_URL is required");
    let _exclusive = lock_exclusive(&database_url).await;
    let pool = device_attestation::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let base = format!("dotnstest{}", std::process::id());

    // `claim_dotns_due` scans the whole table. This suite's rows must be the
    // only claimable ones. The suite owns the dedicated test DB, so clearing is
    // safe.
    sqlx::query("DELETE FROM username_reservations WHERE dotns_status IS NOT NULL OR base LIKE 'dotnstest%'")
        .execute(&pool)
        .await
        .expect("pre-clean reservations");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(LEASE)
        .execute(&pool)
        .await
        .expect("pre-clean lease");

    // Three rows inserted. One in the lane. One with no dotns block. One in the
    // lane but still waiting on People.
    let in_lane = outbox::insert(&pool, &reservation(&base, "11", true))
        .await
        .expect("insert with dotns");
    let no_block = outbox::insert(&pool, &reservation(&base, "12", false))
        .await
        .expect("insert without dotns");
    let not_yet_assigned = outbox::insert(&pool, &reservation(&base, "13", true))
        .await
        .expect("insert with dotns");

    // A dotns block puts the row in the lane at insert. Its absence leaves NULL.
    assert_eq!(
        dotns_status(&pool, in_lane).await.as_deref(),
        Some(DotnsStatus::Pending.as_str())
    );
    assert_eq!(dotns_status(&pool, no_block).await, None);

    // Nothing is claimable yet. None of the rows are ASSIGNED on People.
    assert!(outbox::claim_dotns_due(&pool, 50)
        .await
        .expect("claim")
        .is_empty());

    assign_on_people(&pool, in_lane).await;
    assign_on_people(&pool, no_block).await;

    // ASSIGNED + PENDING is claimable. ASSIGNED with no dotns block is not.
    // Neither is the PENDING row still waiting on People.
    let due = outbox::claim_dotns_due(&pool, 50).await.expect("claim");
    assert_eq!(due.iter().map(|r| r.id).collect::<Vec<_>>(), vec![in_lane]);
    assert!(!due.iter().any(|r| r.id == no_block));
    assert!(!due.iter().any(|r| r.id == not_yet_assigned));

    // The claimed row carries the payload the writer needs.
    let claimed = &due[0];
    assert_eq!(claimed.dotns_signature.as_deref(), Some(&[6u8; 64][..]));
    assert_eq!(claimed.dotns_signed_at, Some(1_750_000_000));
    assert_eq!(claimed.dotns_attempt, 0);

    // Transitions need a held lease.
    let epoch = lease::try_acquire(&pool, LEASE, "holder-a", Duration::from_secs(30))
        .await
        .expect("acquire lease")
        .expect("lease is free");
    let guard = Guard {
        lease_name: LEASE.to_string(),
        holder_id: "holder-a".to_string(),
        epoch,
    };

    assert!(
        outbox::mark_dotns_submitting(&pool, &guard, in_lane, "0xdeadbeef", 1)
            .await
            .expect("mark submitting")
    );
    assert_eq!(
        dotns_status(&pool, in_lane).await.as_deref(),
        Some(DotnsStatus::Submitting.as_str())
    );
    assert_eq!(
        outbox::dotns_submitting(&pool)
            .await
            .expect("submitting scan")
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![in_lane]
    );

    // A terminal dotNS failure leaves the People registration ASSIGNED. The
    // username works even though the dotNS name does not.
    assert!(
        outbox::mark_dotns_failed(&pool, &guard, in_lane, "dotns signature does not verify")
            .await
            .expect("mark failed")
    );
    assert_eq!(
        dotns_status(&pool, in_lane).await.as_deref(),
        Some(DotnsStatus::FailedTerminal.as_str())
    );
    assert_eq!(
        people_status(&pool, in_lane).await,
        Status::Assigned.as_str()
    );
    // ...and a terminal row is never claimed again.
    assert!(outbox::claim_dotns_due(&pool, 50)
        .await
        .expect("claim")
        .is_empty());

    // A stale writer that lost the lease cannot advance anything.
    let stale = Guard {
        lease_name: LEASE.to_string(),
        holder_id: "holder-b".to_string(),
        epoch: epoch + 99,
    };
    assert!(!outbox::mark_dotns_reserved(&pool, &stale, in_lane)
        .await
        .expect("stale write"));
    assert_eq!(
        dotns_status(&pool, in_lane).await.as_deref(),
        Some(DotnsStatus::FailedTerminal.as_str())
    );

    // Expiry is its own terminal state. It is distinct from FAILED_TERMINAL.
    assign_on_people(&pool, not_yet_assigned).await;
    assert!(
        outbox::mark_dotns_expired(&pool, &guard, not_yet_assigned, "signature expired")
            .await
            .expect("mark expired")
    );
    assert_eq!(
        dotns_status(&pool, not_yet_assigned).await.as_deref(),
        Some(DotnsStatus::Expired.as_str())
    );

    // The gauge pass reports every status, including the empty ones. It never
    // counts rows outside the lane.
    let depths = outbox::dotns_depth_by_status(&pool)
        .await
        .expect("dotns depths");
    assert_eq!(depths.len(), DotnsStatus::ALL.len());
    let total: i64 = depths.iter().map(|(_, d)| d.depth).sum();
    assert_eq!(total, 2, "only the two rows carrying a dotns block");

    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean up");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(LEASE)
        .execute(&pool)
        .await
        .expect("clean up lease");
}

/// A terminal People failure closes the dotNS lane on the same row.
///
/// `claim_dotns_due` only takes rows already `ASSIGNED` on People, so a dotNS
/// row left `PENDING` under a dead People half is unreachable forever — while
/// still being counted by `dotns_depth_by_status`. That is what turns the
/// `PENDING` depth and oldest-age gauges into a permanent false "stuck lane".
#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn a_terminal_people_failure_abandons_the_dotns_lane() {
    let database_url = std::env::var("DEVICE_ATTESTATION_TEST_DATABASE_URL")
        .expect("DEVICE_ATTESTATION_TEST_DATABASE_URL is required");
    let _exclusive = lock_exclusive(&database_url).await;
    let pool = device_attestation::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let base = format!("dotnsdead{}", std::process::id());

    sqlx::query("DELETE FROM username_reservations WHERE base LIKE 'dotnsdead%'")
        .execute(&pool)
        .await
        .expect("pre-clean");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(LEASE)
        .execute(&pool)
        .await
        .expect("pre-clean lease");

    // One row per dotNS state a terminal People failure has to reason about.
    let pending = outbox::insert(&pool, &reservation(&base, "31", true))
        .await
        .expect("insert pending");
    let retrying = outbox::insert(&pool, &reservation(&base, "32", true))
        .await
        .expect("insert retrying");
    let submitting = outbox::insert(&pool, &reservation(&base, "33", true))
        .await
        .expect("insert submitting");
    let reserved = outbox::insert(&pool, &reservation(&base, "34", true))
        .await
        .expect("insert reserved");
    let no_block = outbox::insert(&pool, &reservation(&base, "35", false))
        .await
        .expect("insert without dotns");

    let epoch = lease::try_acquire(&pool, LEASE, "holder-a", Duration::from_secs(30))
        .await
        .expect("acquire lease")
        .expect("lease is free");
    let guard = Guard {
        lease_name: LEASE.to_string(),
        holder_id: "holder-a".to_string(),
        epoch,
    };

    // Drive the three non-PENDING rows into their states. Marking dotNS needs
    // the People half ASSIGNED first, which mark_failed then overwrites — the
    // realistic ordering for `retrying`/`submitting` is a People failure after
    // the dotNS half has already moved.
    for id in [retrying, submitting, reserved] {
        assign_on_people(&pool, id).await;
    }
    let later = time::OffsetDateTime::now_utc() + Duration::from_secs(600);
    assert!(
        outbox::mark_dotns_retry(&pool, &guard, retrying, later, 1, "transient")
            .await
            .expect("mark retry")
    );
    assert!(
        outbox::mark_dotns_submitting(&pool, &guard, submitting, "0xfeed", 1)
            .await
            .expect("mark submitting")
    );
    assert!(outbox::mark_dotns_reserved(&pool, &guard, reserved)
        .await
        .expect("mark reserved"));

    for id in [pending, retrying, submitting, reserved, no_block] {
        assert!(
            outbox::mark_failed(&pool, &guard, id, "candidate signature rejected on chain")
                .await
                .expect("fail on people")
        );
        assert_eq!(
            people_status(&pool, id).await,
            Status::FailedTerminal.as_str()
        );
    }

    // The two open states are closed out...
    assert_eq!(
        dotns_status(&pool, pending).await.as_deref(),
        Some(DotnsStatus::Abandoned.as_str())
    );
    assert_eq!(
        dotns_status(&pool, retrying).await.as_deref(),
        Some(DotnsStatus::Abandoned.as_str())
    );
    // ...with a reason that distinguishes them from a real dotNS rejection.
    let reason: Option<String> =
        sqlx::query("SELECT dotns_last_error FROM username_reservations WHERE id = $1")
            .bind(pending)
            .fetch_one(&pool)
            .await
            .expect("read dotns_last_error")
            .try_get("dotns_last_error")
            .expect("decode dotns_last_error");
    assert_eq!(
        reason.as_deref(),
        Some("People registration failed terminally; dotNS reservation never attempted")
    );
    // The retry gate is cleared too, so nothing looks merely deferred.
    let not_before: Option<time::OffsetDateTime> =
        sqlx::query("SELECT dotns_not_before FROM username_reservations WHERE id = $1")
            .bind(retrying)
            .fetch_one(&pool)
            .await
            .expect("read dotns_not_before")
            .try_get("dotns_not_before")
            .expect("decode dotns_not_before");
    assert!(
        not_before.is_none(),
        "an abandoned row is not waiting on a backoff"
    );

    // SUBMITTING is left alone: that extrinsic is already broadcast, and the
    // LiteLabelOwner reconcile owns its outcome. Closing it here would lose a
    // reservation that may well have landed.
    assert_eq!(
        dotns_status(&pool, submitting).await.as_deref(),
        Some(DotnsStatus::Submitting.as_str())
    );
    // Terminal success is not rewritten into a failure-shaped state.
    assert_eq!(
        dotns_status(&pool, reserved).await.as_deref(),
        Some(DotnsStatus::Reserved.as_str())
    );
    // A row that was never in the lane stays out of it.
    assert_eq!(dotns_status(&pool, no_block).await, None);

    // The gauge no longer reports a PENDING row that nothing can ever claim.
    let depths = outbox::dotns_depth_by_status(&pool)
        .await
        .expect("dotns depths");
    let depth_of = |status: DotnsStatus| {
        depths
            .iter()
            .find(|(s, _)| *s == status)
            .map(|(_, d)| d.depth)
            .expect("every status is reported")
    };
    assert_eq!(depth_of(DotnsStatus::Pending), 0);
    assert_eq!(depth_of(DotnsStatus::RetryAfter), 0);
    assert_eq!(depth_of(DotnsStatus::Abandoned), 2);

    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean up");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(LEASE)
        .execute(&pool)
        .await
        .expect("clean up lease");
}

/// The no-backfill property. A row that predates the lane has `dotns_status`
/// NULL. It is never claimed, however far it advances on People.
#[tokio::test]
#[ignore = "requires Postgres; set DEVICE_ATTESTATION_TEST_DATABASE_URL and run with --ignored"]
async fn rows_predating_the_lane_are_never_submitted() {
    let database_url = std::env::var("DEVICE_ATTESTATION_TEST_DATABASE_URL")
        .expect("DEVICE_ATTESTATION_TEST_DATABASE_URL is required");
    let _exclusive = lock_exclusive(&database_url).await;
    let pool = device_attestation::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let base = format!("dotnsold{}", std::process::id());

    sqlx::query("DELETE FROM username_reservations WHERE base LIKE 'dotnsold%'")
        .execute(&pool)
        .await
        .expect("pre-clean");

    // Simulating a pre-migration row. dotns payload stored, lane column NULL.
    let id = outbox::insert(&pool, &reservation(&base, "21", true))
        .await
        .expect("insert");
    sqlx::query(
        "UPDATE username_reservations SET dotns_status = NULL, status = 'ASSIGNED' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("clear dotns_status");

    assert!(
        !outbox::claim_dotns_due(&pool, 50)
            .await
            .expect("claim")
            .iter()
            .any(|r| r.id == id),
        "a NULL dotns_status row must never enter the lane"
    );

    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean up");
}
