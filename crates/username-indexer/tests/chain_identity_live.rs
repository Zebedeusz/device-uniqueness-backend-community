//! Live-Postgres coverage for the chain-identity guard in
//! [`username_indexer::bootstrap::ensure_seeded`]: the projection is discarded
//! when the connected chain is not the one it was built from, adopted when the
//! checkpoint predates the `genesis_hash` column, and left alone otherwise.
//!
//! All three branches are reachable with a dead (mock) chain client, whose
//! genesis hash is the all-zero hash: the guard decides before any storage read
//! and only the discard path goes on to attempt a rescan. That failing rescan
//! is the point of step 3 — the discard must already be committed, or a chain
//! that is briefly unreachable after a wipe would leave the stale checkpoint in
//! place and the guard would have changed nothing.
//!
//! `sync_state` is a singleton row (id = 1), so every step runs sequentially
//! inside ONE test and the original row is restored at the end. Kept out of
//! `ingest_live` for the same reason: two suites mutating that row must not
//! interleave (`scripts/run_live_suites.sh` runs suites one at a time).
//!
//! `#[ignore]`d like the other live suites; `DATABASE_URL` is required:
//!
//! ```sh
//! DATABASE_URL=postgres://username_indexer:username_indexer@localhost:5433/username_indexer \
//!   cargo test -p username-indexer --test chain_identity_live -- --ignored
//! ```

use std::sync::Arc;

use sqlx::{PgPool, Row as _};
use username_indexer::bootstrap;
use username_indexer::chain::ChainClient;

/// The mock client answers every `chain_getBlockHash` with the all-zero hash,
/// so the genesis hash subxt captures at construction is this value.
const MOCK_GENESIS: [u8; 32] = [0u8; 32];

/// A genesis hash no chain in this test will report — "some other network".
const FOREIGN_GENESIS: [u8; 32] = [0xffu8; 32];

/// Distinctive enough that a leaked row is obviously this suite's.
const TEST_ACCOUNT: [u8; 32] = [0xa7u8; 32];

/// An offline People Chain client; answers only the genesis handshake, so any
/// actual read fails — proving the guard decides before touching the chain.
async fn dead_chain_client() -> ChainClient {
    use subxt_rpcs::client::mock_rpc_client::Json;
    use subxt_rpcs::client::{MockRpcClient, RpcClient};

    let mock = MockRpcClient::builder()
        .method_handler("chain_getBlockHash", |_params| async {
            Json(serde_json::json!(format!("0x{}", "00".repeat(32))))
        })
        .build();
    let backend = subxt::backend::LegacyBackend::builder().build(RpcClient::new(mock));
    let client = subxt::OnlineClient::<chain_types::PeopleConfig>::from_backend(Arc::new(backend))
        .await
        .expect("offline client from mock backend");
    ChainClient::from_online(client)
}

/// Write the singleton checkpoint with an explicit chain stamp (`None` = a row
/// written before the `genesis_hash` column existed).
async fn set_checkpoint(pool: &PgPool, number: i64, genesis: Option<[u8; 32]>) {
    sqlx::query(
        "INSERT INTO sync_state (
            id, last_finalized_number, last_finalized_hash, genesis_hash,
            last_synced_at, records_indexed, decode_failures, updated_at
         ) VALUES (1, $1, $2, $3, now(), 0, 0, now())
         ON CONFLICT (id) DO UPDATE SET
            last_finalized_number = EXCLUDED.last_finalized_number,
            genesis_hash = EXCLUDED.genesis_hash,
            updated_at = now()",
    )
    .bind(number)
    .bind(vec![0u8; 32])
    .bind(genesis.map(|hash| hash.to_vec()))
    .execute(pool)
    .await
    .expect("seed sync_state");
}

/// One projection row, standing in for everything derived from the chain.
async fn seed_projection_row(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO assigned_usernames (
            account_id, account_id_ss58, identifier_key, lite_username, lite_base,
            lite_digits, display_username, snapshot_hash, snapshot_number
         ) VALUES ($1, 'test-ss58', $2, 'guardtest.1', 'guardtest', 1, 'guardtest.1', $3, 1)
         ON CONFLICT (account_id) DO NOTHING",
    )
    .bind(TEST_ACCOUNT.as_slice())
    .bind(vec![0u8; 65])
    .bind(vec![0u8; 32])
    .execute(pool)
    .await
    .expect("seed projection row");
}

async fn stored_genesis(pool: &PgPool) -> Option<Vec<u8>> {
    sqlx::query("SELECT genesis_hash FROM sync_state WHERE id = 1")
        .fetch_optional(pool)
        .await
        .expect("read sync_state")
        .and_then(|row| row.try_get("genesis_hash").expect("genesis_hash column"))
}

async fn projection_rows(pool: &PgPool) -> i64 {
    sqlx::query("SELECT count(*) FROM assigned_usernames")
        .fetch_one(pool)
        .await
        .expect("count projection")
        .try_get(0)
        .expect("count")
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL Postgres; run manually with --ignored"]
async fn chain_identity_guard_discards_only_a_foreign_projection() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    let chain = dead_chain_client().await;

    // Preserve whatever checkpoint the shared test DB had.
    let original =
        sqlx::query("SELECT last_finalized_number, genesis_hash FROM sync_state WHERE id = 1")
            .fetch_optional(&pool)
            .await
            .expect("read original");

    // 1. A checkpoint written before the column existed is adopted, not
    //    rebuilt: the deploy that adds the guard must not wipe every healthy
    //    projection it finds.
    set_checkpoint(&pool, 42, None).await;
    seed_projection_row(&pool).await;
    assert!(
        bootstrap::ensure_seeded(&pool, &chain, 64)
            .await
            .expect("ensure_seeded adopts an unstamped checkpoint")
            .is_none(),
        "an adopted checkpoint must not trigger a bootstrap"
    );
    assert_eq!(
        stored_genesis(&pool).await.as_deref(),
        Some(MOCK_GENESIS.as_slice()),
        "adoption stamps the live chain's genesis"
    );
    assert!(projection_rows(&pool).await > 0, "adoption keeps the rows");

    // 2. A checkpoint already stamped with this chain resumes untouched — the
    //    steady-state restart path, still without a chain read.
    assert!(bootstrap::ensure_seeded(&pool, &chain, 64)
        .await
        .expect("ensure_seeded")
        .is_none());
    assert_eq!(
        stored_genesis(&pool).await.as_deref(),
        Some(MOCK_GENESIS.as_slice())
    );
    assert!(projection_rows(&pool).await > 0);

    // 3. A checkpoint from another chain is discarded with the projection built
    //    from it. The rescan that follows fails against the dead chain, so this
    //    also pins the ordering: the discard commits independently, rather than
    //    being rolled back into "resume on the stale checkpoint".
    set_checkpoint(&pool, 42, Some(FOREIGN_GENESIS)).await;
    seed_projection_row(&pool).await;
    assert!(
        bootstrap::ensure_seeded(&pool, &chain, 64).await.is_err(),
        "the rebuild must be attempted, and fails on the dead chain"
    );
    assert!(
        stored_genesis(&pool).await.is_none(),
        "the foreign checkpoint row is gone, so the next boot bootstraps"
    );
    assert_eq!(
        projection_rows(&pool).await,
        0,
        "rows derived from a chain we are no longer on are discarded"
    );

    // Restore the shared DB's original checkpoint state.
    sqlx::query("DELETE FROM assigned_usernames WHERE account_id = $1")
        .bind(TEST_ACCOUNT.as_slice())
        .execute(&pool)
        .await
        .expect("clean projection row");
    match original {
        Some(row) => {
            let genesis: Option<Vec<u8>> = row.try_get("genesis_hash").expect("genesis_hash");
            let restored = genesis.map(|bytes| {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&bytes);
                hash
            });
            set_checkpoint(
                &pool,
                row.try_get("last_finalized_number").expect("number"),
                restored,
            )
            .await;
        }
        None => {
            sqlx::query("DELETE FROM sync_state WHERE id = 1")
                .execute(&pool)
                .await
                .expect("restore empty");
        }
    }
}
