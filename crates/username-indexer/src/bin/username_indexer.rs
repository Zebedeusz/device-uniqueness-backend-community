//! One-shot finalized username bootstrap followed by the username-indexer server.

use std::time::Duration;

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use username_indexer::http::middleware::RateLimiter;
use username_indexer::poc::{self, Poc};
use username_indexer::sync::{self, Freshness, FreshnessSnapshot};
use username_indexer::{db, ensure_seeded, routes, AppState, ChainClient, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env().context("invalid configuration")?;
    tracing::info!(
        bind = %config.bind_addr,
        people_rpc = %config.people_rpc_url,
        storage_page_size = config.storage_page_size,
        sync_interval_secs = config.sync_interval_secs,
        "starting username-indexer"
    );

    let pool = db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url, config.storage_page_size).await?;

    let freshness = Freshness::new();
    match ensure_seeded(&pool, &chain, config.storage_page_size).await? {
        Some(report) => {
            tracing::info!(
                indexed = report.indexed,
                skipped = report.skipped,
                snapshot_number = report.snapshot_number,
                "finalized username bootstrap complete"
            );
            freshness.update(FreshnessSnapshot {
                last_finalized_number: report.snapshot_number,
                last_synced_at: time::OffsetDateTime::now_utc(),
                records_indexed: report.indexed,
                decode_failures: report.skipped,
            });
        }
        None => {
            if let Some(snapshot) = sync::checkpoint_freshness(&pool).await? {
                tracing::info!(
                    last_finalized_number = snapshot.last_finalized_number,
                    "existing checkpoint found; resuming incremental sync (skipping full bootstrap)"
                );
                freshness.update(snapshot);
            }
        }
    }
    tokio::spawn(sync::run(
        pool.clone(),
        chain.clone(),
        config.clone(),
        freshness.clone(),
    ));

    let bind_addr = config.bind_addr;
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "username-indexer listening");

    let limiter = RateLimiter::new(
        config.search_rate_limit,
        Duration::from_secs(config.search_rate_limit_window_secs.into()),
    );
    let mut state = AppState::new(pool.clone(), chain, freshness, limiter);
    if let Some(poc) = build_poc(&config)? {
        tracing::info!(
            difficulty_bits = config.poc_difficulty_bits,
            session_ttl_secs = config.poc_session_ttl_secs,
            clock_skew_secs = config.poc_clock_skew_secs,
            "proof-of-compute gate enabled on public search (a valid JWT also satisfies it)"
        );
        tokio::spawn(prune_spent_puzzles(
            pool.clone(),
            Duration::from_secs(config.poc_session_ttl_secs.into()),
        ));
        state = state.with_poc(poc);
    }
    axum::serve(listener, routes(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

/// Build the proof-of-compute gate when it is enabled.
///
/// Requires verify-only JWT material (`JWT_JWKS_JSON` or
/// `JWT_ED25519_PUBLIC_KEY`) so authenticated callers keep bypassing the puzzle;
/// missing key material is a boot failure rather than a silent gate that would
/// force the shipping apps to mine.
fn build_poc(config: &Config) -> anyhow::Result<Option<Poc>> {
    let Some(secret) = config.poc_hmac_secret.as_deref() else {
        return Ok(None);
    };
    let verifier = http_common::config::jwt_verifier_from_env()
        .context("POC_ENABLED=true requires verify-only JWT material")?;
    Ok(Some(Poc::new(
        poc::derive_secret(secret),
        config.poc_difficulty_bits,
        config.poc_session_ttl_secs,
        config.poc_clock_skew_secs,
        verifier,
    )))
}

/// Periodically drop spent-puzzle rows whose validity window has passed.
async fn prune_spent_puzzles(pool: sqlx::PgPool, every: Duration) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        interval.tick().await;
        match poc::store::prune_expired(&pool).await {
            Ok(0) => {}
            Ok(pruned) => tracing::debug!(pruned, "pruned expired spent puzzles"),
            Err(error) => tracing::warn!(%error, "pruning expired spent puzzles failed"),
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received; draining connections");
}
