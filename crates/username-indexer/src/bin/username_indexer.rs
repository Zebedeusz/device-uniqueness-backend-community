//! One-shot finalized username bootstrap followed by the username-indexer server.

use std::time::Duration;

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use username_indexer::http::middleware::RateLimiter;
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
    axum::serve(
        listener,
        routes(AppState::new(pool, chain, freshness, limiter)),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")?;
    Ok(())
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
