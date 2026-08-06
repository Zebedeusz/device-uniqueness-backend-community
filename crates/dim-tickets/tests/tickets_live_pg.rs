//! Live-Postgres tests for the `dim_tickets` store (`src/tickets.rs`) and the
//! `writer_lease` acquire/heartbeat/epoch semantics (`src/writer.rs`).
//!
//! These need a real database, which the workspace deliberately keeps out of
//! CI, so they are `#[ignore]`d. Run them locally against any scratch
//! Postgres (the compose `dim-tickets-postgres` publishes localhost:5434 with
//! `docker-compose.debug.yml`):
//!
//! ```sh
//! DIM_TICKETS_TEST_DATABASE_URL=postgres://dim_tickets:dim_tickets@localhost:5434/dim_tickets \
//!   cargo test -p dim-tickets --test tickets_live_pg -- --ignored
//! ```
//!
//! Concurrency: cargo runs test fns on parallel threads, so the suite never
//! truncates shared tables (that wipes sibling fixtures — the claim_live_pg
//! lesson). Isolation comes from unique per-test row keys instead: every
//! ticket, inviter, and lease name is minted fresh per call, and each test
//! deletes its own rows on the way out. No `--test-threads=1` needed.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dim_tickets::tickets::{self, Dim, Network, TicketRow};
use dim_tickets::writer::{heartbeat_lease, try_acquire_lease};
use sqlx::Row as _;
use time::OffsetDateTime;

async fn test_pool() -> sqlx::PgPool {
    let url = std::env::var("DIM_TICKETS_TEST_DATABASE_URL")
        .expect("set DIM_TICKETS_TEST_DATABASE_URL to run the live-PG tests");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect to test Postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    pool
}

/// Unique per-call key (row PK, inviter, lease name, holder id) minted from
/// pid, nanos, and a counter, so parallel tests — and leftovers from a
/// crashed earlier run — can never collide.
fn unique(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_timestamp_nanos(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Insert a fresh PENDING ticket, failing the test on a conflict.
async fn seed(pool: &sqlx::PgPool, ticket: &str, inviter: &str, dim: Dim) -> TicketRow {
    tickets::insert_ticket(pool, ticket, inviter, Network::Paseo, dim)
        .await
        .expect("insert ticket")
        .expect("fresh ticket must not conflict")
}

/// The row's writer-side columns, which `TicketRow` deliberately omits.
async fn raw_row(
    pool: &sqlx::PgPool,
    ticket: &str,
) -> (
    String,
    i32,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Option<serde_json::Value>,
) {
    let row = sqlx::query(
        "SELECT status, retry_count, retry_at, updated_at, onchain_data \
         FROM dim_tickets WHERE ticket = $1",
    )
    .bind(ticket)
    .fetch_one(pool)
    .await
    .expect("row exists");
    (
        row.try_get("status").expect("status"),
        row.try_get("retry_count").expect("retry_count"),
        row.try_get("retry_at").expect("retry_at"),
        row.try_get("updated_at").expect("updated_at"),
        row.try_get("onchain_data").expect("onchain_data"),
    )
}

/// Pin `created_at` explicitly — FIFO assertions must not ride on the
/// microsecond spacing of consecutive inserts.
async fn set_created_at(pool: &sqlx::PgPool, ticket: &str, at: OffsetDateTime) {
    sqlx::query("UPDATE dim_tickets SET created_at = $2 WHERE ticket = $1")
        .bind(ticket)
        .bind(at)
        .execute(pool)
        .await
        .expect("pin created_at");
}

async fn set_status(pool: &sqlx::PgPool, ticket: &str, status: &str) {
    sqlx::query("UPDATE dim_tickets SET status = $2 WHERE ticket = $1")
        .bind(ticket)
        .bind(status)
        .execute(pool)
        .await
        .expect("set status");
}

async fn set_retry_at(pool: &sqlx::PgPool, ticket: &str, at: OffsetDateTime) {
    sqlx::query("UPDATE dim_tickets SET retry_at = $2 WHERE ticket = $1")
        .bind(ticket)
        .bind(at)
        .execute(pool)
        .await
        .expect("set retry_at");
}

/// Self-clean: each test owns exactly the tickets of its unique inviter(s).
async fn clean_inviter(pool: &sqlx::PgPool, inviter: &str) {
    sqlx::query("DELETE FROM dim_tickets WHERE inviter = $1")
        .bind(inviter)
        .execute(pool)
        .await
        .expect("clean up test rows");
}

async fn clean_lease(pool: &sqlx::PgPool, name: &str) {
    sqlx::query("DELETE FROM writer_lease WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await
        .expect("clean up lease row");
}

/// Postgres stores timestamptz at microsecond precision; compare accordingly.
fn close_to(a: OffsetDateTime, b: OffsetDateTime) -> bool {
    (a - b).abs() < time::Duration::milliseconds(1)
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn insert_returns_the_pending_row_and_none_on_conflict() {
    let pool = test_pool().await;
    let (ticket, inviter) = (unique("tkt"), unique("inv"));

    let row = seed(&pool, &ticket, &inviter, Dim::Game).await;
    assert_eq!(row.ticket, ticket);
    assert_eq!(row.inviter, inviter);
    assert_eq!(row.network, "paseo");
    assert_eq!(row.dim, "Game");
    assert_eq!(row.status, "PENDING");
    assert!(row.onchain_data.is_none());
    assert!(row.updated_at.is_none());

    // The PK is the ticket alone: a second insert loses even under a
    // different inviter/network/dim, and the original row is untouched.
    let other_inviter = unique("inv");
    let conflict = tickets::insert_ticket(
        &pool,
        &ticket,
        &other_inviter,
        Network::Westend2,
        Dim::ProofOfInk,
    )
    .await
    .expect("insert");
    assert!(conflict.is_none());
    let kept = tickets::get_ticket(&pool, &ticket)
        .await
        .expect("get")
        .expect("row still there");
    assert_eq!(kept.inviter, inviter);
    assert_eq!(kept.network, "paseo");
    assert_eq!(kept.dim, "Game");

    clean_inviter(&pool, &inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn get_ticket_round_trips_the_row_and_misses_are_none() {
    let pool = test_pool().await;
    let (ticket, inviter) = (unique("tkt"), unique("inv"));

    let miss = tickets::get_ticket(&pool, &ticket).await.expect("get");
    assert!(miss.is_none());

    let inserted = seed(&pool, &ticket, &inviter, Dim::ProofOfInk).await;
    let got = tickets::get_ticket(&pool, &ticket)
        .await
        .expect("get")
        .expect("hit");
    assert_eq!(got.ticket, inserted.ticket);
    assert_eq!(got.inviter, inserted.inviter);
    assert_eq!(got.network, inserted.network);
    assert_eq!(got.dim, "ProofOfInk");
    assert_eq!(got.status, "PENDING");
    assert!(close_to(got.created_at, inserted.created_at));

    clean_inviter(&pool, &inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn fetch_due_is_fifo_by_created_at_within_the_limit() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let (a, b, c) = (unique("tkt"), unique("tkt"), unique("tkt"));
    let t0 = OffsetDateTime::now_utc() - time::Duration::minutes(10);
    for (ticket, offset_secs) in [(&a, 2i64), (&b, 0), (&c, 1)] {
        seed(&pool, ticket, &inviter, Dim::Game).await;
        set_created_at(&pool, ticket, t0 + time::Duration::seconds(offset_secs)).await;
    }

    let now = OffsetDateTime::now_utc();
    let due = tickets::fetch_due(&pool, now, 10, Network::Paseo, &inviter)
        .await
        .expect("fetch_due");
    let order: Vec<&str> = due.iter().map(|r| r.ticket.as_str()).collect();
    assert_eq!(order, vec![b.as_str(), c.as_str(), a.as_str()]);

    // The limit truncates the tail, never reorders the front.
    let due = tickets::fetch_due(&pool, now, 2, Network::Paseo, &inviter)
        .await
        .expect("fetch_due");
    let order: Vec<&str> = due.iter().map(|r| r.ticket.as_str()).collect();
    assert_eq!(order, vec![b.as_str(), c.as_str()]);

    clean_inviter(&pool, &inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn fetch_due_filters_status_lease_network_and_inviter() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let other_inviter = unique("inv");
    let now = OffsetDateTime::now_utc();

    // Due: PENDING with no retry_at, PENDING whose retry_at has passed, and
    // an in-flight orphan whose lease has expired.
    let due_pending = unique("tkt");
    seed(&pool, &due_pending, &inviter, Dim::Game).await;
    let due_retry = unique("tkt");
    seed(&pool, &due_retry, &inviter, Dim::Game).await;
    set_retry_at(&pool, &due_retry, now - time::Duration::seconds(60)).await;
    let orphan = unique("tkt");
    seed(&pool, &orphan, &inviter, Dim::Game).await;
    set_status(&pool, &orphan, "SUBMITTED").await;
    set_retry_at(&pool, &orphan, now - time::Duration::seconds(60)).await;

    // Not due: a live SUBMITTING lease, terminal statuses, other partitions.
    let leased = unique("tkt");
    seed(&pool, &leased, &inviter, Dim::Game).await;
    set_status(&pool, &leased, "SUBMITTING").await;
    set_retry_at(&pool, &leased, now + time::Duration::seconds(60)).await;
    let registered = unique("tkt");
    seed(&pool, &registered, &inviter, Dim::Game).await;
    set_status(&pool, &registered, "REGISTERED").await;
    let failed = unique("tkt");
    seed(&pool, &failed, &inviter, Dim::Game).await;
    set_status(&pool, &failed, "FAILED").await;
    let other_network = unique("tkt");
    tickets::insert_ticket(
        &pool,
        &other_network,
        &inviter,
        Network::Westend2,
        Dim::Game,
    )
    .await
    .expect("insert")
    .expect("fresh");
    let foreign = unique("tkt");
    seed(&pool, &foreign, &other_inviter, Dim::Game).await;

    let due = tickets::fetch_due(&pool, now, 100, Network::Paseo, &inviter)
        .await
        .expect("fetch_due");
    let mut got: Vec<(&str, &str)> = due
        .iter()
        .map(|r| (r.ticket.as_str(), r.status.as_str()))
        .collect();
    got.sort_unstable();
    let mut want = vec![
        (due_pending.as_str(), "PENDING"),
        (due_retry.as_str(), "PENDING"),
        (orphan.as_str(), "SUBMITTED"), // raw status: the planner maps it
    ];
    want.sort_unstable();
    assert_eq!(got, want);

    clean_inviter(&pool, &inviter).await;
    clean_inviter(&pool, &other_inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn mark_failed_is_terminal_and_scoped_to_the_inviter_partition() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let other_inviter = unique("inv");
    let mine = unique("tkt");
    seed(&pool, &mine, &inviter, Dim::Game).await;
    let foreign = unique("tkt");
    seed(&pool, &foreign, &other_inviter, Dim::Game).await;

    // The foreign ticket is in the list but outside the partition: only ours
    // may flip.
    tickets::mark_failed(
        &pool,
        &[mine.clone(), foreign.clone()],
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("mark_failed");

    let (status, _, _, updated_at, _) = raw_row(&pool, &mine).await;
    assert_eq!(status, "FAILED");
    assert!(updated_at.is_some(), "FAILED must stamp updated_at");
    let (status, _, _, updated_at, _) = raw_row(&pool, &foreign).await;
    assert_eq!(status, "PENDING");
    assert!(updated_at.is_none());

    clean_inviter(&pool, &inviter).await;
    clean_inviter(&pool, &other_inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn mark_submitting_leases_the_batch_and_recover_orphaned_releases_it() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let (a, b) = (unique("tkt"), unique("tkt"));
    seed(&pool, &a, &inviter, Dim::Game).await;
    seed(&pool, &b, &inviter, Dim::Game).await;

    let lease_until = OffsetDateTime::now_utc() + time::Duration::seconds(90);
    tickets::mark_submitting(
        &pool,
        &[a.clone(), b.clone()],
        lease_until,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("mark_submitting");
    for ticket in [&a, &b] {
        let (status, _, retry_at, updated_at, _) = raw_row(&pool, ticket).await;
        assert_eq!(status, "SUBMITTING");
        assert!(close_to(retry_at.expect("lease set"), lease_until));
        assert!(updated_at.is_some());
    }

    // Recovery resets only the named orphans: PENDING again, lease cleared.
    tickets::recover_orphaned(&pool, std::slice::from_ref(&a), Network::Paseo, &inviter)
        .await
        .expect("recover_orphaned");
    let (status, _, retry_at, _, _) = raw_row(&pool, &a).await;
    assert_eq!(status, "PENDING");
    assert!(retry_at.is_none(), "recovery must clear the lease");
    let (status, _, retry_at, _, _) = raw_row(&pool, &b).await;
    assert_eq!(status, "SUBMITTING");
    assert!(retry_at.is_some());

    clean_inviter(&pool, &inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn mark_registered_sets_onchain_data_without_touching_updated_at() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let ticket = unique("tkt");
    seed(&pool, &ticket, &inviter, Dim::Game).await;

    // Give updated_at a real submission-time value first, as a tick would.
    let lease_until = OffsetDateTime::now_utc() + time::Duration::seconds(90);
    tickets::mark_submitting(
        &pool,
        std::slice::from_ref(&ticket),
        lease_until,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("mark_submitting");
    let (_, _, _, submitted_at, _) = raw_row(&pool, &ticket).await;
    let submitted_at = submitted_at.expect("submitting stamps updated_at");

    let onchain = serde_json::json!({ "blockHash": "0xabc", "blockNumber": 7 });
    tickets::mark_registered(
        &pool,
        std::slice::from_ref(&ticket),
        &onchain,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("mark_registered");

    let (status, _, _, updated_at, onchain_data) = raw_row(&pool, &ticket).await;
    assert_eq!(status, "REGISTERED");
    assert_eq!(onchain_data, Some(onchain));
    // The deliberate legacy quirk: registration does NOT touch updated_at, so
    // the wire's updatedAt keeps reporting the submission time.
    assert_eq!(updated_at, Some(submitted_at));

    clean_inviter(&pool, &inviter).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn schedule_retry_requeues_with_count_and_backoff_lease() {
    let pool = test_pool().await;
    let inviter = unique("inv");
    let ticket = unique("tkt");
    seed(&pool, &ticket, &inviter, Dim::Game).await;
    let lease_until = OffsetDateTime::now_utc() + time::Duration::seconds(90);
    tickets::mark_submitting(
        &pool,
        std::slice::from_ref(&ticket),
        lease_until,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("mark_submitting");

    let retry_at = OffsetDateTime::now_utc() + time::Duration::seconds(8);
    tickets::schedule_retry(&pool, &ticket, 3, retry_at, Network::Paseo, &inviter)
        .await
        .expect("schedule_retry");

    let (status, retry_count, db_retry_at, updated_at, _) = raw_row(&pool, &ticket).await;
    assert_eq!(status, "PENDING");
    assert_eq!(retry_count, 3);
    assert!(close_to(db_retry_at.expect("backoff lease"), retry_at));
    assert!(updated_at.is_some());

    // Until retry_at passes the row is not due; afterwards it is.
    let early = tickets::fetch_due(
        &pool,
        retry_at - time::Duration::seconds(1),
        10,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("fetch_due");
    assert!(early.is_empty());
    let late = tickets::fetch_due(
        &pool,
        retry_at + time::Duration::seconds(1),
        10,
        Network::Paseo,
        &inviter,
    )
    .await
    .expect("fetch_due");
    let got: Vec<(&str, i32)> = late
        .iter()
        .map(|r| (r.ticket.as_str(), r.retry_count))
        .collect();
    assert_eq!(got, vec![(ticket.as_str(), 3)]);

    clean_inviter(&pool, &inviter).await;
}

/// Force the lease's expiry into the past so takeover paths run without
/// sleeping through a real TTL.
async fn expire_lease(pool: &sqlx::PgPool, name: &str) {
    sqlx::query("UPDATE writer_lease SET expires_at = now() - interval '1 second' WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await
        .expect("expire lease");
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn lease_acquire_is_exclusive_until_expiry_and_every_take_bumps_the_epoch() {
    let pool = test_pool().await;
    let name = unique("lease");
    let (alpha, beta) = (unique("holder"), unique("holder"));

    // Fresh lease: first holder takes epoch 1.
    let epoch = try_acquire_lease(&pool, &name, &alpha, 60)
        .await
        .expect("acquire");
    assert_eq!(epoch, Some(1));

    // A live lease blocks every other holder.
    let blocked = try_acquire_lease(&pool, &name, &beta, 60)
        .await
        .expect("acquire");
    assert_eq!(blocked, None);

    // The holder itself may re-acquire (deploy restart), bumping the epoch.
    let renewed = try_acquire_lease(&pool, &name, &alpha, 60)
        .await
        .expect("acquire");
    assert_eq!(renewed, Some(2));

    // Once expired, another holder takes over — with a fresh epoch, so the
    // old holder's guard can never heartbeat again.
    expire_lease(&pool, &name).await;
    let taken = try_acquire_lease(&pool, &name, &beta, 60)
        .await
        .expect("acquire");
    assert_eq!(taken, Some(3));

    clean_lease(&pool, &name).await;
}

#[tokio::test]
#[ignore = "requires Postgres; set DIM_TICKETS_TEST_DATABASE_URL and run with --ignored"]
async fn lease_heartbeat_renews_only_the_current_holder_and_epoch() {
    let pool = test_pool().await;
    let name = unique("lease");
    let (alpha, beta) = (unique("holder"), unique("holder"));
    let epoch = try_acquire_lease(&pool, &name, &alpha, 60)
        .await
        .expect("acquire")
        .expect("fresh lease");

    let expires_before: OffsetDateTime =
        sqlx::query("SELECT expires_at FROM writer_lease WHERE name = $1")
            .bind(&name)
            .fetch_one(&pool)
            .await
            .expect("lease row")
            .try_get("expires_at")
            .expect("expires_at");

    // The rightful holder+epoch renews and pushes expiry forward.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(heartbeat_lease(&pool, &name, &alpha, epoch, 60)
        .await
        .expect("heartbeat"));
    let expires_after: OffsetDateTime =
        sqlx::query("SELECT expires_at FROM writer_lease WHERE name = $1")
            .bind(&name)
            .fetch_one(&pool)
            .await
            .expect("lease row")
            .try_get("expires_at")
            .expect("expires_at");
    assert!(expires_after > expires_before);

    // Wrong epoch or wrong holder: no renewal.
    assert!(!heartbeat_lease(&pool, &name, &alpha, epoch - 1, 60)
        .await
        .expect("heartbeat"));
    assert!(!heartbeat_lease(&pool, &name, &beta, epoch, 60)
        .await
        .expect("heartbeat"));

    // After a takeover the old guard is dead and the new one beats.
    expire_lease(&pool, &name).await;
    let new_epoch = try_acquire_lease(&pool, &name, &beta, 60)
        .await
        .expect("acquire")
        .expect("takeover");
    assert!(!heartbeat_lease(&pool, &name, &alpha, epoch, 60)
        .await
        .expect("heartbeat"));
    assert!(heartbeat_lease(&pool, &name, &beta, new_epoch, 60)
        .await
        .expect("heartbeat"));

    clean_lease(&pool, &name).await;
}
