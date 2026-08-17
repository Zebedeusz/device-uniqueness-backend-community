//! `all-in-one` — every HTTP surface in one process, on one port.
//!
//! This is what a consumer runs to get the whole API without an edge in front
//! of it: the route table is [`crate::routes`], compiled in, so nobody has to
//! reconstruct it from a Caddyfile. It also serves `/docs` from the committed
//! API reference, which the edge otherwise file-serves.
//!
//! **Not a deployed workload.** `scripts/verify_role_split.sh` rejects any
//! compose service or chart workload naming this role, because it holds the
//! union of every secret in the system — `JWT_ED25519_SECRET`,
//! `POC_HMAC_SECRET`, and three database URLs — in one address space with the
//! unauthenticated search path. Phase 5 of
//! `docs/plans/active/one-image-one-binary.md` is where that becomes a
//! supported deployment, with the threat model and topology rules that need.
//!
//! The three single-instance workers are **not** part of this role: they hold
//! Postgres leases and nonce lanes and stay separate processes in every
//! topology, so the smallest deployment is this plus those three.

use anyhow::Context as _;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use secrecy::ExposeSecret as _;
use serde_json::json;
use tower_http::services::ServeDir;

use crate::routes::{self, Surfaces};

/// Where the committed API reference lives inside the image. Overridable by the
/// **same** variable the edge uses for its `handle_path /docs*` root
/// (`gateway/Caddyfile`), so the two cannot be configured differently.
const DOCS_ROOT_VAR: &str = "GATEWAY_DOCS_ROOT";
const DEFAULT_DOCS_ROOT: &str = "/srv/docs";

/// Everything the aggregate readiness probe needs, kept beside the router so a
/// component cannot be added to one and forgotten in the other.
#[derive(Clone)]
struct Health {
    identity: (sqlx::PgPool, identity_service::ChainClient),
    indexer: (sqlx::PgPool, username_indexer::ChainClient),
    invite_tickets: invite_tickets::AppState,
    turn: turn::AppState,
    notifications: notifications::AppState,
}

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("all-in-one");
    http_common::metrics::spawn("all-in-one");

    // Each service's own config, read independently. This is the role the
    // config namespacing exists for: before it, three of these read the same
    // `DATABASE_URL` naming three different Postgres instances, so this process
    // would have connected every one of them to whichever database happened to
    // be set.
    let identity_config = identity_service::Config::from_env().context("identity-api config")?;
    let indexer_config = username_indexer::Config::from_env().context("username-indexer config")?;
    let invite_config = invite_tickets::Config::from_env().context("invite-tickets-api config")?;
    let turn_config = turn::Config::from_env().context("turn-api config")?;
    let notify_config = notifications::Config::from_env().context("notify-relay config")?;

    let bind_addr = identity_config.bind_addr;
    tracing::info!(bind = %bind_addr, "starting all-in-one: five surfaces, one port");

    // Three migration sets against three databases, before anything serves. One
    // bad migration blocks the whole API here, where in the eight-workload
    // topology it blocks one service — the demo asserts it fails loudly.
    let identity_pool =
        identity_service::db::connect(identity_config.database_url.expose_secret()).await?;
    let indexer_pool = username_indexer::db::connect(&indexer_config.database_url).await?;
    let invite_pool = invite_tickets::db::connect(&invite_config.database_url).await?;

    let identity_chain =
        identity_service::ChainClient::connect(&identity_config.people_rpc_url).await?;
    let indexer_chain = username_indexer::ChainClient::connect(
        &indexer_config.people_rpc_url,
        indexer_config.storage_page_size,
    )
    .await?;
    tracing::info!("connected to People Chain");

    // identity
    let jwt = identity_service::Jwt::new(
        identity_config.jwt_secret.expose_secret(),
        identity_config.jwt_issuer.clone(),
    );
    let identity_state = identity_service::AppState::new(
        identity_pool.clone(),
        identity_chain.clone(),
        jwt,
        identity_config,
    );

    // username-indexer: the bootstrap and the incremental sync run here too, or
    // search would serve an empty projection.
    let indexer_state = crate::roles::username_indexer::build_state(
        &indexer_config,
        indexer_pool.clone(),
        indexer_chain.clone(),
    )
    .await?;

    let invite_state = invite_tickets::AppState::new(invite_pool, invite_config);
    let turn_state = turn::AppState::new(turn_config);
    let notify_state = crate::roles::notify_relay::build_state(notify_config)?;

    let health = Health {
        identity: (identity_pool, identity_chain),
        indexer: (indexer_pool, indexer_chain),
        invite_tickets: invite_state.clone(),
        turn: turn_state.clone(),
        notifications: notify_state.clone(),
    };

    let app = routes::merge(Surfaces {
        identity: identity_service::http::router(identity_state),
        indexer: username_indexer::http::router(indexer_state),
        invite_tickets: invite_tickets::http::router(invite_state),
        turn: turn::http::router(turn_state),
        notifications: notifications::http::router(notify_state),
    });

    let app = http_common::layers::standard_layers(
        Router::new()
            .merge(health_router().with_state(health))
            // The edge file-serves this from the same committed artifact; with
            // no edge in front, the process serves it itself.
            .nest_service("/docs", ServeDir::new(docs_root()))
            .merge(app),
    );

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "all-in-one listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::drain())
        .await
        .context("server error")?;
    Ok(())
}

fn docs_root() -> String {
    std::env::var(DOCS_ROOT_VAR).unwrap_or_else(|_| DEFAULT_DOCS_ROOT.to_string())
}

/// Health for the merged process. Five services' probes behind one `/readyz`, so
/// a dead dependency is visible in one response rather than only on a port
/// nobody polls.
fn health_router() -> Router<Health> {
    Router::new()
        .route(
            "/healthcheck",
            get(|| async { Json(json!({ "status": "ok", "service": "all-in-one" })) }),
        )
        .route(
            "/livez",
            get(|| async { Json(json!({ "status": "alive", "service": "all-in-one" })) }),
        )
        .route("/readyz", get(readyz))
}

/// Aggregate readiness: report every component, and stay **ready while
/// degraded**.
///
/// This is the decision Phase 5 of `docs/plans/active/one-image-one-binary.md`
/// owed, and it is deliberately the opposite of the per-service behaviour.
///
/// Readiness controls whether the instance receives traffic **at all**. With five
/// surfaces behind one probe, a strict aggregate turns "the ticket service's
/// database is down" into "the whole API is out of rotation, auth included" — a
/// partial outage promoted to a total one. And in this topology there is a
/// single workload, so there is no healthy replica for the refused traffic to
/// move to: refusing it buys nothing and costs everything still working.
///
/// So: `200` with `"status": "degraded"` and the dead components named, until
/// nothing is serving. A surface whose dependency is down fails its own
/// requests, which is where that failure belongs. `/livez` stays a liveness echo
/// in both topologies — a dependency outage must never restart the process, or a
/// database blip becomes a crash loop.
async fn readyz(State(health): State<Health>) -> Response {
    let (identity_pool, identity_chain) = &health.identity;
    let (indexer_pool, indexer_chain) = &health.indexer;

    let results: [(&str, http_common::health::Readiness); 5] = [
        (
            "identity-api",
            identity_service::http::health::probe(identity_pool.clone(), identity_chain.clone())
                .await,
        ),
        (
            "username-indexer",
            username_indexer::http::health::probe(indexer_pool.clone(), indexer_chain.clone())
                .await,
        ),
        (
            "invite-tickets-api",
            invite_tickets::http::readiness(health.invite_tickets.clone()).await,
        ),
        ("turn-api", turn::http::readiness(health.turn.clone()).await),
        (
            "notify-relay",
            notifications::http::readiness(health.notifications.clone()).await,
        ),
    ];

    let results_len = results.len();
    let mut body = json!({ "status": "ready", "service": "all-in-one" });
    let mut ready = true;
    let mut degraded = 0usize;
    for (service, outcome) in results {
        match outcome {
            Ok(components) => {
                let mut up = serde_json::Map::new();
                for component in components {
                    up.insert((*component).to_string(), json!("up"));
                }
                body[service] = serde_json::Value::Object(up);
            }
            Err(component) => {
                ready = false;
                degraded += 1;
                body[service] = json!({ component: "down" });
            }
        }
    }

    if ready {
        (StatusCode::OK, Json(body)).into_response()
    } else if degraded < results_len {
        // Something is still serving. Report the damage, keep taking traffic.
        body["status"] = json!("degraded");
        (StatusCode::OK, Json(body)).into_response()
    } else {
        // Every surface is down: there is nothing to protect by staying in
        // rotation, and an operator polling /readyz should see it plainly.
        body["status"] = json!("unavailable");
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}
