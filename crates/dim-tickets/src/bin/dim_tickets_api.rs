//! `dim-tickets-api` — the N-replica axum server for the DIM ticket service.
//!
//! Serves health plus the frozen `/api/v1/dim-ticket` surface (request +
//! status). Writes land as `PENDING` rows; the separate single-instance
//! `dim-tickets-writer` binary registers them on People Chain (Phase 2).

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use dim_tickets::{db, routes, AppState, ChainClient, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    http_common::metrics::spawn("dim-tickets-api");

    let config = Config::from_env().context("invalid configuration")?;
    tracing::info!(
        bind = %config.bind_addr,
        network = config.network.as_str(),
        people_rpc = %config.people_rpc_url,
        inviter = %config.inviter_address,
        "starting dim-tickets-api"
    );

    let pool = db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    tracing::info!("connected to People Chain");

    let bind_addr = config.bind_addr;
    let state = AppState::new(pool, chain, config);
    http_common::metrics::spawn_readiness_probe(
        "dim-tickets-api",
        state.clone(),
        dim_tickets::http::readiness,
    );
    let app = routes(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "dim-tickets-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// Resolve on Ctrl-C or (on Unix) `SIGTERM` so axum can drain in-flight
/// requests before the process exits.
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
