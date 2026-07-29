//! Manual-only live-Postgres test for the proof-of-compute one-shot consume.
//! NOT part of `just check`.
//!
//! The gate's accepted-solution and replay paths are the only ones that touch
//! the database, so they cannot be replayed offline like the rest of the gate
//! contract (`poc_gate.rs`). This test is `#[ignore]`d and additionally
//! short-circuits when `DATABASE_URL` is unset. Run it by hand against a
//! disposable Postgres:
//!
//! ```sh
//! DATABASE_URL=postgres://username_indexer:username_indexer@localhost:5433/username_indexer \
//!   cargo test -p username-indexer --test poc_gate_live -- --ignored
//! ```
//!
//! It applies the crate migrations, then asserts that a solved puzzle is
//! consumed exactly once — including across two independently built gates, which
//! is the property legacy's per-process map could not provide.

use sqlx::postgres::PgPoolOptions;
use username_indexer::poc::puzzle::{checksum_hex, derive_secret};
use username_indexer::poc::solution::mine;
use username_indexer::poc::{now_millis, Poc, Rejection, Solution};
use uuid::Uuid;

const IKM: &str = "test-poc-ikm";
const DIFFICULTY: u8 = 8;
const TTL_SECS: u32 = 60;
const SKEW_SECS: u32 = 30;

fn gate() -> Poc {
    // A throwaway verifier: this test drives the puzzle lane, never the bypass.
    let verifying_key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    Poc::new(
        derive_secret(IKM),
        DIFFICULTY,
        TTL_SECS,
        SKEW_SECS,
        jwt_verify::Verifier::from_verifying_key(verifying_key),
    )
}

fn solved(timestamp_ms: i64) -> Solution {
    let session_id = Uuid::new_v4();
    Solution::new(
        session_id,
        timestamp_ms,
        DIFFICULTY,
        mine(session_id, timestamp_ms, DIFFICULTY),
        checksum_hex(&derive_secret(IKM), session_id, timestamp_ms, DIFFICULTY),
    )
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL Postgres; run manually with --ignored"]
async fn a_solved_puzzle_is_consumed_exactly_once() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset; skipping live proof-of-compute test");
        return;
    };

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations apply");

    let now = now_millis();
    let solution = solved(now);

    // First presentation: accepted.
    assert_eq!(
        gate()
            .verify(&pool, &solution, now)
            .await
            .expect("no database error"),
        Ok(())
    );

    // Replay against a *different* gate instance — a second replica sharing the
    // same database — is refused.
    assert_eq!(
        gate()
            .verify(&pool, &solution, now)
            .await
            .expect("no database error"),
        Err(Rejection::Replayed)
    );

    // A fresh puzzle still works, so the consume is per-session and not a
    // global latch.
    let other = solved(now);
    assert_eq!(
        gate()
            .verify(&pool, &other, now)
            .await
            .expect("no database error"),
        Ok(())
    );

    // Pruning removes only rows past their retention window.
    let retained = username_indexer::poc::store::prune_expired(&pool)
        .await
        .expect("prune runs");
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM spent_puzzles")
        .fetch_one(&pool)
        .await
        .expect("count rows");
    assert!(
        remaining >= 2,
        "the two fresh sessions must survive pruning (pruned {retained}, remaining {remaining})"
    );

    sqlx::query("DELETE FROM spent_puzzles WHERE session_id = ANY($1)")
        .bind(vec![solution.session_id(), other.session_id()])
        .execute(&pool)
        .await
        .expect("cleanup");
}
