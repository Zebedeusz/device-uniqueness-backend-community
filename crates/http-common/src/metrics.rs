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
        let mut probe_order: &'static [&'static str] = &[];
        loop {
            interval.tick().await;
            probe_order = record_readiness(ready(state.clone()).await, probe_order);
        }
    });
    tracing::info!(
        service,
        every_secs = READINESS_PROBE_INTERVAL.as_secs(),
        "readiness gauge probe started"
    );
}

/// Publish one readiness verdict and return the probe's component order.
///
/// The probes short-circuit in report order, so a failure proves the failed
/// component down and everything probed before it up — both are recorded, so a
/// component that recovered while a later one broke does not stay stuck at 0.
/// Components *after* the failure were never checked, and are published as
/// `NaN`: the gauge would otherwise keep its last value, which reads as "up"
/// and is the difference between "the database is fine" and "nobody looked".
/// `probe_order` is the order from the most recent healthy probe.
fn record_readiness(
    readiness: crate::health::Readiness,
    probe_order: &'static [&'static str],
) -> &'static [&'static str] {
    for (component, value) in readiness_plan(readiness, probe_order) {
        metrics::gauge!("ibv2_dependency_up", "component" => component).set(value);
    }
    match readiness {
        Ok(components) => {
            metrics::gauge!("ibv2_readyz_ok").set(1.0);
            components
        }
        Err(_) => {
            metrics::gauge!("ibv2_readyz_ok").set(0.0);
            probe_order
        }
    }
}

/// The pure half of [`record_readiness`]: which `ibv2_dependency_up` series to
/// set, and to what — `1.0` up, `0.0` down, `NaN` not checked.
fn readiness_plan(
    readiness: crate::health::Readiness,
    probe_order: &'static [&'static str],
) -> Vec<(&'static str, f64)> {
    match readiness {
        Ok(components) => components.iter().map(|c| (*c, 1.0)).collect(),
        Err(failed) => {
            let mut plan = Vec::with_capacity(probe_order.len() + 1);
            let mut after_failure = false;
            for component in probe_order {
                if *component == failed {
                    after_failure = true;
                    continue;
                }
                plan.push((*component, if after_failure { f64::NAN } else { 1.0 }));
            }
            // Last, and unconditional: the failed component may not be in
            // `probe_order` at all before the first healthy probe.
            plan.push((failed, 0.0));
            plan
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
    parse_enabled(std::env::var("METRICS_ENABLED").ok().as_deref())
}

/// The pure half of [`enabled`]: only an explicit falsey value disables.
fn parse_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0" | "n"
        ),
        None => true,
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
    fn a_healthy_probe_marks_every_component_up() {
        const ORDER: &[&str] = &["db", "chain"];
        assert_eq!(
            readiness_plan(Ok(ORDER), &[]),
            vec![("db", 1.0), ("chain", 1.0)]
        );
    }

    #[test]
    fn components_after_the_failure_are_not_claimed_up() {
        const ORDER: &[&str] = &["db", "chain", "rpc"];
        let plan = readiness_plan(Err("chain"), ORDER);

        assert_eq!(plan[0], ("db", 1.0), "probed before the failure: proven up");
        assert_eq!(plan[1].0, "rpc");
        assert!(
            plan[1].1.is_nan(),
            "never checked, so it must not report 1.0: {:?}",
            plan[1]
        );
        assert_eq!(plan[2], ("chain", 0.0), "the failure itself");
        assert_eq!(plan.len(), 3);
    }

    #[test]
    fn a_failure_before_any_healthy_probe_still_reports_the_component() {
        assert_eq!(readiness_plan(Err("db"), &[]), vec![("db", 0.0)]);
    }

    #[test]
    fn default_addr_is_the_metrics_port() {
        let addr: SocketAddr = DEFAULT_METRICS_ADDR.parse().expect("valid default");
        assert_eq!(addr.port(), 9090);
    }

    #[test]
    fn only_explicit_falsey_values_disable_metrics() {
        for off in ["false", "FALSE", "no", "off", "0", "n"] {
            assert!(!parse_enabled(Some(off)), "{off:?} should disable metrics");
        }
        for on in ["true", "1", "yes", "sure", ""] {
            assert!(
                parse_enabled(Some(on)),
                "{on:?} should leave metrics enabled"
            );
        }
        assert!(parse_enabled(None), "unset should leave metrics enabled");
    }
}
