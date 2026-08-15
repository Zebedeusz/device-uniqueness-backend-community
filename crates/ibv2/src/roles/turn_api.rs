//! `turn-api` — the stateless N-replica axum server for the TURN credential
//! issuer.
//!
//! Serves health plus the frozen `POST /api/v1/turn/issue` surface. No
//! database, no chain: the only secret is the HMAC key shared with the TURN
//! relay, and replicas need no coordination.

use anyhow::Context as _;
use turn::{routes, AppState, Config};

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("turn-api");
    http_common::metrics::spawn("turn-api");

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
    http_common::metrics::spawn_readiness_probe("turn-api", state.clone(), turn::http::readiness);
    let app = routes(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "turn-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::drain())
        .await
        .context("server error")?;

    Ok(())
}
