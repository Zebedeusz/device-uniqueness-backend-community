//! `device-attestation-api` — the N-replica axum server for Device Uniqueness Backend.
//!
//! Serves health, JWKS, and the `/api/v1/auth/*` handshake plus the
//! `/api/v1/usernames*` and `/api/v1/attester` surface. Registration writes land
//! in the reservation outbox; the separate `device-attestation-chain-writer` role drains them
//! onto People Chain.

use anyhow::Context as _;
use secrecy::ExposeSecret as _;

use device_attestation::{db, routes, AppState, ChainClient, Config, Jwt};

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("device-attestation-api");
    http_common::metrics::spawn("device-attestation-api");

    let config = Config::from_env().context("invalid configuration")?;
    tracing::info!(
        bind = %config.bind_addr,
        issuer = %config.jwt_issuer,
        people_rpc = %config.people_rpc_url,
        attestation = config.attestation_mode(),
        "starting device-attestation-api"
    );

    let jwt = Jwt::new(config.jwt_secret.expose_secret(), config.jwt_issuer.clone());
    let pool = db::connect(config.database_url.expose_secret()).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    tracing::info!("connected to People Chain");

    let bind_addr = config.bind_addr;
    let state = AppState::new(pool, chain, jwt, config);
    let (probe_pool, probe_chain) = (state.pool.clone(), state.chain.clone());
    http_common::metrics::spawn_readiness_probe(
        "device-attestation-api",
        (probe_pool, probe_chain),
        |(p, c)| device_attestation::http::health::probe(p, c),
    );
    let app = routes(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "device-attestation-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::drain())
        .await
        .context("server error")?;

    Ok(())
}
