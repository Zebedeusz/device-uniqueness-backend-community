//! Live-Postgres test for the maintainer's per-tick advisory lock: while a
//! [`invite_tickets::pool::acquire_tick_lock`] guard is held on one
//! connection, a second connection's `pg_try_advisory_lock` on the same key
//! must fail; once the guard drops (its `Drop` spawns the async unlock), the
//! second connection can take the lock.
//!
//! Needs a real database, which the workspace deliberately keeps out of CI,
//! so it is `#[ignore]`d. Run it locally against any scratch Postgres:
//!
//! ```sh
//! INVITE_TICKETS_TEST_DATABASE_URL=postgres://user:pass@localhost:5432/invite_test \
//!   cargo test -p invite-tickets --test pool_live_pg -- --ignored
//! ```
//!
//! Unlike `claim_live_pg` this suite touches no tables (advisory locks only),
//! and its single test needs no `--test-threads=1`. It shares the fixed lock
//! key with any maintainer pointed at the same database, so both acquisition
//! phases poll rather than demanding the lock instantly; a maintainer holding
//! the lock for minutes (chain finalization in flight) can still starve it.

use std::time::Duration;

use invite_tickets::pool::{acquire_tick_lock, POOL_LOCK_KEY};

async fn test_pool() -> sqlx::PgPool {
    let url = std::env::var("INVITE_TICKETS_TEST_DATABASE_URL")
        .expect("set INVITE_TICKETS_TEST_DATABASE_URL to run the live-PG tests");
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect to test Postgres")
}

#[tokio::test]
#[ignore = "needs INVITE_TICKETS_TEST_DATABASE_URL (live Postgres)"]
async fn tick_lock_excludes_second_connection_until_guard_drops() {
    let pool = test_pool().await;

    // Connection B first, so the guard's own connection is a distinct session.
    let mut probe = pool.acquire().await.expect("acquire probe connection");

    // Connection A: hold the tick lock via the guard. Poll: a maintainer on
    // the same database holds the key briefly every tick.
    let mut guard = None;
    for _ in 0..300 {
        if let Some(taken) = acquire_tick_lock(&pool).await.expect("acquire_tick_lock") {
            guard = Some(taken);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let guard = guard.expect("tick lock never became free (long-held by another process?)");

    // A second maintainer instance would see the lock as taken.
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(POOL_LOCK_KEY)
        .fetch_one(&mut *probe)
        .await
        .expect("probe try-lock");
    assert!(!taken, "held guard must exclude a second connection");

    // Dropping the guard spawns the async unlock; poll until it lands.
    drop(guard);
    let mut reacquired = false;
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(POOL_LOCK_KEY)
            .fetch_one(&mut *probe)
            .await
            .expect("probe try-lock after drop");
        if taken {
            reacquired = true;
            break;
        }
    }
    assert!(
        reacquired,
        "dropped guard must release the lock for the next holder"
    );

    // Leave the key free for reruns (session cleanup would also release it).
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(POOL_LOCK_KEY)
        .execute(&mut *probe)
        .await
        .expect("release probe lock");
}
