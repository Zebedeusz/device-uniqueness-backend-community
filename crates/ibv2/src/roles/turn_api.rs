//! `turn-api` — the stateless N-replica axum server for the TURN credential
//! issuer.
//!
//! Serves health plus the frozen `POST /api/v1/turn/issue` surface. No
//! database, and no chain unless `TURN_PROOF_ENABLED` is on — that mounts
//! `issue-with-proof` and a background People Chain ring-root refresher.
//! Otherwise the only secret is the HMAC key shared with the TURN relay, and
//! replicas need no coordination.

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
    let app = routes(state.clone());

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "turn-api listening");

    // The root refresher starts only after the listener is bound: a dead
    // chain must not stop `turn-api` booting or serving `/turn/issue`.
    if let (Some(proof_state), Some(proof_config)) = (&state.proof, &state.config.proof) {
        for collection in turn::proof::roots::PersonhoodCollection::ALL {
            turn::proof::roots::spawn_refresher(
                proof_state.roots.get(collection),
                turn::proof::roots::RootsConfig {
                    rpc_url: proof_config.rpc_url.clone(),
                    collection: collection.id(),
                    genesis: proof_config.genesis,
                    refresh: turn::config::PROOF_ROOT_REFRESH,
                },
            );
        }
        tracing::info!("proof-authorized issuance enabled; root refreshers started");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::drain())
        .await
        .context("server error")?;

    Ok(())
}
