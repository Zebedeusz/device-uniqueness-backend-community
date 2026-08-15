//! `dim-tickets-api` — the N-replica axum server for the DIM ticket service.
//!
//! Serves health plus the frozen `/api/v1/dim-ticket` surface (request +
//! status). Writes land as `PENDING` rows; the separate single-instance
//! `dim-tickets-writer` role registers them on People Chain.

use anyhow::Context as _;

use dim_tickets::{db, routes, AppState, ChainClient, Config};

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("dim-tickets-api");
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
        .with_graceful_shutdown(crate::shutdown::drain())
        .await
        .context("server error")?;

    Ok(())
}
