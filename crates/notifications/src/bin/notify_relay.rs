//! `notify-relay` — the stateless notifications relay server.
//!
//! Verifies identity-service JWTs with configured public-key material and serves
//! `POST /api/v1/notify` plus health endpoints. iOS uses the real APNs provider
//! when `APNS_*` is configured and Android the real FCM provider when
//! `FCM_SERVICE_ACCOUNT_JSON` is set; each falls back to an unconfigured stub.

use std::sync::Arc;

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use notifications::{
    routes, ApnsConfig, ApnsProvider, AppState, Config, FcmConfig, FcmProvider, PushProvider,
    RateLimiter, UnconfiguredProvider,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    http_common::metrics::spawn("notify-relay");

    let config = Config::from_env().context("invalid configuration")?;
    tracing::info!(bind = %config.bind_addr, "starting notify-relay");

    let apns = build_apns_provider()?;
    let fcm = build_fcm_provider()?;
    let limiter = RateLimiter::new(config.rate_limit, config.rate_window);
    tracing::info!(
        limit = config.rate_limit,
        window_secs = config.rate_window.as_secs(),
        "per-subject notify rate limit configured"
    );
    let bind_addr = config.bind_addr;
    let state = AppState::new(config.jwt_verifier, apns, fcm, limiter);
    http_common::metrics::spawn_readiness_probe(
        "notify-relay",
        state.clone(),
        notifications::http::readiness,
    );

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %bind_addr, "notify-relay listening");

    axum::serve(listener, routes(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

/// Build the iOS provider: real APNs when configured, else an unconfigured stub.
fn build_apns_provider() -> anyhow::Result<Arc<dyn PushProvider>> {
    match ApnsConfig::from_env().context("invalid APNs configuration")? {
        Some(config) => {
            tracing::info!(
                environment = ?config.environment,
                topic = %config.topic,
                "APNs provider configured"
            );
            Ok(Arc::new(
                ApnsProvider::new(config).context("building APNs provider")?,
            ))
        }
        None => {
            tracing::info!("APNs not configured; iOS pushes will report provider failure");
            Ok(Arc::new(UnconfiguredProvider))
        }
    }
}

/// Build the Android provider: real FCM when configured, else an unconfigured stub.
fn build_fcm_provider() -> anyhow::Result<Arc<dyn PushProvider>> {
    match FcmConfig::from_env().context("invalid FCM configuration")? {
        Some(config) => {
            tracing::info!(project = %config.project_id, "FCM provider configured");
            Ok(Arc::new(
                FcmProvider::new(config).context("building FCM provider")?,
            ))
        }
        None => {
            tracing::info!("FCM not configured; Android pushes will report provider failure");
            Ok(Arc::new(UnconfiguredProvider))
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
