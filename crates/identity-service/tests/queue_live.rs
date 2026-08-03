//! Live-Postgres coverage for the registration queue: queued intake,
//! slot-rule promotion (`advance_iteration`), the FIFO snapshot, the
//! stranded-queue liveness signal, and the flag-off janitor drain.

use identity_service::chain::lease;
use identity_service::chain::outbox::{self, NewReservation};
use identity_service::queue;
use sqlx::Row as _;

fn reservation(base: &str, digits: &str, account: &str) -> NewReservation {
    NewReservation {
        account_id: account.to_string(),
        candidate_account_id: "test-candidate".to_string(),
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

#[tokio::test]
#[ignore = "requires Postgres; set IDENTITY_TEST_DATABASE_URL and run with --ignored"]
async fn advancer_promotes_by_slot_rules_until_the_queue_drains() {
    let database_url = std::env::var("IDENTITY_TEST_DATABASE_URL")
        .expect("IDENTITY_TEST_DATABASE_URL is required");
    let pool = identity_service::db::connect(&database_url)
        .await
        .expect("connect and migrate");
    let base = format!("queuetest{}", std::process::id());

    // Self-clean before starting, not just at the end: the slot-order and
    // drain assertions require this test's rows to be the ONLY QUEUED rows
    // (`advance_iteration` / `fallback_drain` operate on the whole table),
    // and an aborted earlier run leaves rows plus a lease behind. This suite
    // owns the dedicated test DB, so clearing them up front is safe.
    sqlx::query(
        "DELETE FROM username_reservations WHERE status = 'QUEUED' OR base LIKE 'queuetest%'",
    )
    .execute(&pool)
    .await
    .expect("pre-clean reservations");
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(queue::ADVANCER_LEASE_NAME)
        .execute(&pool)
        .await
        .expect("pre-clean advancer lease");

    // Enqueue order (FIFO): groups 1, 2, 3, 4, 4, 1 — six claims, two subjects
    // sharing group 4 vs distinct low-priority subjects.
    let groups: [i32; 6] = [1, 2, 3, 4, 4, 1];
    let mut ids = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let digits = format!("{:02}", index + 11);
        let account = format!("queue-subject-{index}");
        let id = outbox::insert_queued(&pool, &reservation(&base, &digits, &account), group)
            .await
            .expect("insert queued");
        ids.push(id);
    }

    // The snapshot sees exactly our rows, FIFO, with their groups.
    let snapshot: Vec<_> = queue::queued_snapshot(&pool)
        .await
        .expect("snapshot")
        .into_iter()
        .filter(|entry| ids.contains(&entry.id))
        .collect();
    assert_eq!(
        snapshot.iter().map(|e| (e.id, e.group)).collect::<Vec<_>>(),
        ids.iter()
            .zip(groups)
            .map(|(&id, group)| (id, group as u8))
            .collect::<Vec<_>>()
    );

    // Iteration 1: slot 1 (G4) -> first g4 claim (ids[3]); slot 2 (G3+G4) ->
    // g3 claim (ids[2], earliest remaining eligible); slot 3 (G2+) -> g2
    // (ids[1]); slot 4 (all) -> oldest overall (ids[0]).
    let first = queue::advance_iteration(&pool).await.expect("iteration 1");
    assert_eq!(
        first.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[3], ids[2], ids[1], ids[0]]
    );

    // Iteration 2: slot 1 -> the remaining g4 claim (ids[4]); slots 2-3 have
    // no eligible groups left and are skipped; slot 4 -> the last g1 (ids[5]).
    let second = queue::advance_iteration(&pool).await.expect("iteration 2");
    assert_eq!(
        second.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[4], ids[5]]
    );
    assert_eq!(second[0].slot, 1);
    assert_eq!(second[1].slot, 4);

    // The in-memory simulation (what `queuePosition` / iteration estimates
    // are computed from) must agree with the SQL promotion order on the SAME
    // data: every promoted id's estimated (position, iteration) equals what
    // `advance_iteration` actually did.
    let observed: Vec<(i64, u32)> = first
        .iter()
        .map(|p| (p.id, 1))
        .chain(second.iter().map(|p| (p.id, 2)))
        .collect();
    for (index, (id, iteration)) in observed.iter().enumerate() {
        let estimate = queue::drain_estimate(&snapshot, *id).expect("was queued");
        assert_eq!(
            estimate.position,
            index as u32 + 1,
            "simulation position matches the SQL promotion order for id {id}"
        );
        assert_eq!(
            estimate.iterations, *iteration,
            "simulation iteration matches the SQL iteration for id {id}"
        );
    }

    // All promoted rows are RESERVED — writer-visible, queue-invisible.
    let statuses = sqlx::query("SELECT DISTINCT status FROM username_reservations WHERE base = $1")
        .bind(&base)
        .fetch_all(&pool)
        .await
        .expect("statuses");
    assert_eq!(statuses.len(), 1);
    assert_eq!(
        statuses[0].try_get::<String, _>("status").expect("status"),
        "RESERVED"
    );

    // --- queue-service liveness: the stranded warning signal + the
    // flag-off janitor drain (sequential with the phases above:
    // fallback_drain is a global mutation) -------------------------------
    //
    // With the queue ENABLED the writer never calls `fallback_drain` — rows
    // park behind the throttle and `stranded_queued` feeds the operator
    // warning. The drain assertions below are the QUEUE_ENABLED=false
    // janitor semantics (retiring the queue must strand nothing).

    // No advancer lease ROW -> the queue service never ran here (or its state
    // was wiped): the liveness probe reports dead, a queued row counts as
    // stranded, and the janitor drain promotes IMMEDIATELY — the grace window
    // deliberately applies only to an expired-but-present lease (a normal
    // restart/deploy), because absence has no timestamp to measure a grace
    // from.
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(queue::ADVANCER_LEASE_NAME)
        .execute(&pool)
        .await
        .expect("clear advancer lease");
    assert!(!queue::advancer_alive(&pool).await.expect("probe"));

    let stranded = outbox::insert_queued(&pool, &reservation(&base, "17", "queue-subject-x"), 3)
        .await
        .expect("insert stranded");
    assert!(
        queue::stranded_queued(&pool).await.expect("stranded count") >= 1,
        "a queued row behind an absent lease counts as stranded"
    );
    queue::fallback_drain(&pool, std::time::Duration::from_secs(60))
        .await
        .expect("janitor drain");
    let status: String = sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(stranded)
        .fetch_one(&pool)
        .await
        .expect("stranded row")
        .try_get("status")
        .expect("status");
    assert_eq!(
        status, "RESERVED",
        "queue-disabled janitor drains rows behind an absent lease"
    );

    // A live advancer lease: nothing is stranded, and even the janitor's
    // drain holds off (a live advancer drains fairly first).
    let epoch = lease::try_acquire(
        &pool,
        queue::ADVANCER_LEASE_NAME,
        "queue-live-test",
        std::time::Duration::from_secs(60),
    )
    .await
    .expect("acquire advancer lease")
    .expect("lease free");
    assert!(queue::advancer_alive(&pool).await.expect("probe"));

    let waiting = outbox::insert_queued(&pool, &reservation(&base, "18", "queue-subject-y"), 2)
        .await
        .expect("insert waiting");
    assert_eq!(
        queue::stranded_queued(&pool).await.expect("stranded count"),
        0,
        "a live advancer lease means queued rows are draining, not stranded"
    );
    queue::fallback_drain(&pool, std::time::Duration::from_secs(60))
        .await
        .expect("fallback drain with live lease");
    let status: String = sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(waiting)
        .fetch_one(&pool)
        .await
        .expect("waiting row")
        .try_get("status")
        .expect("status");
    assert_eq!(status, "QUEUED", "live queue service keeps claims queued");

    // Expired-but-PRESENT lease (a normal restart/deploy): the row is
    // stranded (warning-worthy) but the janitor's grace window holds the
    // drain off; only a longer outage drains. Renewal must also refuse the
    // expired lease — the holder has to re-acquire a fresh epoch.
    sqlx::query(
        "UPDATE writer_lease SET expires_at = now() - interval '10 seconds' WHERE name = $1",
    )
    .bind(queue::ADVANCER_LEASE_NAME)
    .execute(&pool)
    .await
    .expect("expire advancer lease");
    assert!(!queue::advancer_alive(&pool).await.expect("probe"));
    assert!(
        queue::stranded_queued(&pool).await.expect("stranded count") >= 1,
        "a queued row behind an expired lease counts as stranded"
    );
    assert!(
        !lease::renew(
            &pool,
            queue::ADVANCER_LEASE_NAME,
            "queue-live-test",
            epoch,
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("renew attempt"),
        "renew must refuse an expired lease"
    );

    queue::fallback_drain(&pool, std::time::Duration::from_secs(60))
        .await
        .expect("drain within grace");
    let status: String = sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(waiting)
        .fetch_one(&pool)
        .await
        .expect("waiting row")
        .try_get("status")
        .expect("status");
    assert_eq!(status, "QUEUED", "expired within grace: queue holds");

    queue::fallback_drain(&pool, std::time::Duration::from_secs(5))
        .await
        .expect("drain beyond grace");
    let status: String = sqlx::query("SELECT status FROM username_reservations WHERE id = $1")
        .bind(waiting)
        .fetch_one(&pool)
        .await
        .expect("waiting row")
        .try_get("status")
        .expect("status");
    assert_eq!(status, "RESERVED", "expired beyond grace: queue drains");

    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(queue::ADVANCER_LEASE_NAME)
        .execute(&pool)
        .await
        .expect("clean advancer lease");
    sqlx::query("DELETE FROM username_reservations WHERE base = $1")
        .bind(&base)
        .execute(&pool)
        .await
        .expect("clean test rows");
}
