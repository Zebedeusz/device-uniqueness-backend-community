//! Prometheus metrics exporter, served on its own port.
//!
//! Every deployed process — HTTP service or headless worker — installs the
//! recorder and serves `GET /metrics` on `METRICS_ADDR` (`0.0.0.0:9090`). A
//! separate listener from the public server, because the edge proxies only the
//! API port: metrics stay unreachable from the internet.
//!
//! Best-effort throughout — a failure here is logged, never fatal. Losing
//! observability must not cost availability.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Default metrics listener; reachable only on the container networks.
const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9090";

/// Install the Prometheus recorder and spawn the `/metrics` listener, labelling
/// every metric with `service`.
///
/// Call once, early in `main`: measurements taken before installation are
/// dropped by the no-op default recorder.
pub fn spawn(service: &'static str) {
    if !enabled() {
        tracing::info!(service, "metrics exporter disabled (METRICS_ENABLED)");
        return;
    }
    let addr = match metrics_addr() {
        Ok(addr) => addr,
        Err(raw) => {
            tracing::warn!(service, addr = %raw, "METRICS_ADDR is not a socket address; metrics disabled");
            return;
        }
    };
    let handle = match PrometheusBuilder::new()
        .add_global_label("service", service)
        .install_recorder()
    {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(service, %error, "installing the Prometheus recorder failed; metrics disabled");
            return;
        }
    };
    tokio::spawn(serve(service, addr, handle));
}

/// How often the readiness probe re-checks a service's dependencies.
const READINESS_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Periodically run `ready` — the probe `/readyz` serves — and publish the
/// verdict as `ibv2_readyz_ok` plus `ibv2_dependency_up{component}`.
///
/// Prometheus' `up` only proves the metrics port answered, so a service whose
/// Postgres died still scrapes as `up`. This closes that gap. Skipped when
/// metrics are disabled: the gauges would be dropped anyway.
pub fn spawn_readiness_probe<S, F, Fut>(service: &'static str, state: S, ready: F)
where
    S: Clone + Send + 'static,
    F: Fn(S) -> Fut + Send + 'static,
    Fut: Future<Output = crate::health::Readiness> + Send + 'static,
{
    if !enabled() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(READINESS_PROBE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            record_readiness(ready(state.clone()).await);
        }
    });
    tracing::info!(
        service,
        every_secs = READINESS_PROBE_INTERVAL.as_secs(),
        "readiness gauge probe started"
    );
}

/// Publish one readiness verdict. On failure only the component that broke is
/// reported: the probes short-circuit, so the rest is genuinely unknown.
fn record_readiness(readiness: crate::health::Readiness) {
    match readiness {
        Ok(components) => {
            metrics::gauge!("ibv2_readyz_ok").set(1.0);
            for component in components {
                metrics::gauge!("ibv2_dependency_up", "component" => *component).set(1.0);
            }
        }
        Err(component) => {
            metrics::gauge!("ibv2_readyz_ok").set(0.0);
            metrics::gauge!("ibv2_dependency_up", "component" => component).set(0.0);
        }
    }
}

/// Bind the metrics listener and serve the rendered registry until shutdown.
async fn serve(service: &'static str, addr: SocketAddr, handle: PrometheusHandle) {
    let app = Router::new().route(
        "/metrics",
        get(move || {
            let handle = handle.clone();
            async move { handle.render() }
        }),
    );
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(service, %addr, %error, "binding the metrics listener failed; metrics disabled");
            return;
        }
    };
    tracing::info!(service, %addr, "metrics listening");
    if let Err(error) = axum::serve(listener, app).await {
        tracing::warn!(service, %error, "metrics listener stopped");
    }
}

/// `METRICS_ENABLED`; anything unparseable counts as enabled, since a typo in
/// an observability knob must not silently blind the deployment.
fn enabled() -> bool {
    match std::env::var("METRICS_ENABLED") {
        Ok(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0" | "n"
        ),
        Err(_) => true,
    }
}

fn metrics_addr() -> Result<SocketAddr, String> {
    let raw = std::env::var("METRICS_ADDR").unwrap_or_else(|_| DEFAULT_METRICS_ADDR.to_string());
    raw.trim().parse().map_err(|_| raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_addr_is_the_metrics_port() {
        let addr: SocketAddr = DEFAULT_METRICS_ADDR.parse().expect("valid default");
        assert_eq!(addr.port(), 9090);
    }

    #[test]
    fn only_explicit_falsey_values_disable_metrics() {
        for off in ["false", "FALSE", "no", "off", "0", "n"] {
            std::env::set_var("METRICS_ENABLED", off);
            assert!(!enabled(), "{off:?} should disable metrics");
        }
        for on in ["true", "1", "yes", "sure", ""] {
            std::env::set_var("METRICS_ENABLED", on);
            assert!(enabled(), "{on:?} should leave metrics enabled");
        }
        std::env::remove_var("METRICS_ENABLED");
        assert!(enabled(), "unset should leave metrics enabled");
    }
}
