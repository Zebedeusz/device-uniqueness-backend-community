//! `turn-api` — the stateless N-replica axum server for the TURN credential
//! issuer.
//!
//! Serves health plus the frozen `POST /api/v1/turn/issue` surface. No
//! database, no chain: the only secret is the HMAC key shared with the TURN
//! relay, and replicas need no coordination.

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;
use turn::{routes, AppState, Config};

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
        algorithm = config.algorithm.as_str(),
        ttl_secs = config.ttl_secs,
        servers = config.ice_servers.len(),
        "starting turn-api"
    );

    let bind_addr = config.bind_addr;
    let state = AppState::new(config);
    let app = routes(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "turn-api listening");

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
