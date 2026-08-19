//! Manual-only live-Postgres pagination test. NOT part of `just check`.
//!
//! This test is `#[ignore]`d and additionally short-circuits when `DATABASE_URL`
//! is unset, so the standard gate (`cargo test` / `just check`) never runs it.
//! Run it by hand against a disposable Postgres:
//!
//! ```sh
//! DATABASE_URL=postgres://username_indexer:username_indexer@localhost:5433/username_indexer \
//!   cargo test -p username-indexer --test pagination_live -- --ignored
//! ```
//!
//! It applies the crate migrations, inserts a deterministic set of
//! `assigned_usernames` rows that exercise the
//! `(lower(display_username) COLLATE "C", lite_base, lite_digits numeric, account_id)`
//! ordering (numeric digit ordering `zqbase.2` before `zqbase.10`, plus a
//! full-username shadowing case where two lite bases share one display name),
//! then pages through with a small limit and asserts the global order is stable
//! and no row appears twice across pages. A zero-padded lite suffix
//! (`zqpad.06`) must come back padded — the numeric `lite_digits` column cannot
//! render it. A row with a pre-RFC-0004 P-256 identifier key is seeded first in
//! sort order and must never surface.

use std::collections::{HashMap, HashSet};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use username_indexer::search::search;

/// Synthetic 32-byte account public keys, one repeated byte each. The values
/// are arbitrary.
const ACCOUNTS: [[u8; 32]; 7] = [
    [0xE1; 32], [0xE2; 32], [0xE3; 32], [0xE4; 32], [0xE5; 32], [0xE6; 32], [0xE7; 32],
];

/// RFC-0004 on-chain container: `0x00` X25519 type byte + key + zero padding.
const X25519_KEY: [u8; 65] = [0u8; 65];

/// Pre-RFC-0004 uncompressed P-256 key (SEC1 `0x04` prefix) — search must
/// hide rows carrying one.
const PRE_RFC0004_P256_KEY: [u8; 65] = {
    let mut key = [0x11u8; 65];
    key[0] = 0x04;
    key
};

#[tokio::test]
#[ignore = "requires a live DATABASE_URL Postgres; run manually with --ignored"]
async fn live_pagination_is_stable_and_non_overlapping() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset; skipping live pagination test");
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
        .expect("apply crate migrations");

    cleanup(&pool).await;
    seed(&pool).await;

    let mut cursor: Option<String> = None;
    let mut account_ids: Vec<String> = Vec::new();
    let mut usernames: Vec<String> = Vec::new();
    for _ in 0..16 {
        let mut params = HashMap::from([
            ("prefix".to_string(), "zq".to_string()),
            // Unknown params are ignored (the Android client sends `status`).
            ("status".to_string(), "ASSIGNED".to_string()),
            ("limit".to_string(), "2".to_string()),
        ]);
        if let Some(cursor) = &cursor {
            params.insert("cursor".to_string(), cursor.clone());
        }
        let response = search(&pool, &params).await.expect("search page");
        for row in &response.usernames {
            account_ids.push(row.account_id.clone());
            usernames.push(row.username.clone());
        }
        match response.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    cleanup(&pool).await;

    assert_eq!(
        account_ids,
        [
            "acc-alpha",
            "acc-beta",
            "acc-gamma",
            "acc-pad",
            "acc-team-2",
            "acc-team-10"
        ],
        "global order must be stable across pages"
    );
    assert_eq!(
        usernames,
        [
            "zqalpha.1",
            "zqbeta.1",
            "zqgamma.3",
            "zqpad.06",
            "Zqteam",
            "Zqteam"
        ],
        "numeric digit ordering (2 before 10), full-username shadowing, and the \
         zero-padded chain form must hold"
    );
    let unique: HashSet<&String> = account_ids.iter().collect();
    assert_eq!(
        unique.len(),
        account_ids.len(),
        "no row may appear twice across pages"
    );
}

async fn seed(pool: &PgPool) {
    insert_row(
        pool,
        ACCOUNTS[0],
        "acc-alpha",
        "zqalpha.1",
        "zqalpha",
        "1",
        None,
        X25519_KEY,
    )
    .await;
    insert_row(
        pool,
        ACCOUNTS[1],
        "acc-beta",
        "zqbeta.1",
        "zqbeta",
        "1",
        None,
        X25519_KEY,
    )
    .await;
    insert_row(
        pool,
        ACCOUNTS[2],
        "acc-gamma",
        "zqgamma.3",
        "zqgamma",
        "3",
        None,
        X25519_KEY,
    )
    .await;
    insert_row(
        pool,
        ACCOUNTS[3],
        "acc-team-2",
        "zqbase.2",
        "zqbase",
        "2",
        Some("Zqteam"),
        X25519_KEY,
    )
    .await;
    insert_row(
        pool,
        ACCOUNTS[4],
        "acc-team-10",
        "zqbase.10",
        "zqbase",
        "10",
        Some("Zqteam"),
        X25519_KEY,
    )
    .await;
    // A zero-padded lite suffix: the chain answers only to `zqpad.06`, so the
    // wire string must keep the padding the numeric column throws away.
    insert_row(
        pool,
        ACCOUNTS[6],
        "acc-pad",
        "zqpad.06",
        "zqpad",
        "06",
        None,
        X25519_KEY,
    )
    .await;
    // Matches the `zq` prefix and would sort first, but carries a
    // pre-RFC-0004 P-256 key — search must never surface it.
    insert_row(
        pool,
        ACCOUNTS[5],
        "acc-pre-rfc0004",
        "zqaaa.1",
        "zqaaa",
        "1",
        None,
        PRE_RFC0004_P256_KEY,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn insert_row(
    pool: &PgPool,
    account_id: [u8; 32],
    account_id_ss58: &str,
    lite_username: &str,
    lite_base: &str,
    lite_digits: &str,
    full_username: Option<&str>,
    identifier_key: [u8; 65],
) {
    let display_username = full_username.unwrap_or(lite_username);
    sqlx::query(
        "INSERT INTO assigned_usernames (
            account_id, account_id_ss58, identifier_key, lite_username, lite_base,
            lite_digits, full_username, display_username, snapshot_hash, snapshot_number
         ) VALUES ($1, $2, $3, $4, $5, $6::numeric, $7, $8, $9, $10)",
    )
    .bind(account_id.as_slice())
    .bind(account_id_ss58)
    .bind(identifier_key.as_slice())
    .bind(lite_username)
    .bind(lite_base)
    .bind(lite_digits)
    .bind(full_username)
    .bind(display_username)
    .bind([0u8; 32].as_slice())
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("insert deterministic row");
}

async fn cleanup(pool: &PgPool) {
    for account in ACCOUNTS {
        sqlx::query("DELETE FROM assigned_usernames WHERE account_id = $1")
            .bind(account.as_slice())
            .execute(pool)
            .await
            .expect("delete test row");
    }
}
