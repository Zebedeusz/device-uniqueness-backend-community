//! Process shutdown signalling, shared by every role.
//!
//! Seven of the ten binaries carried a byte-identical copy of this; the eighth
//! (`invite-tickets-pool`) carried the same handler without the drain log,
//! because it logs its own message instead. Hence two entry points rather than
//! one: the log lines every role emitted before this refactor are unchanged.

/// Resolve on Ctrl-C or (on Unix) `SIGTERM` — the signal orchestrators send on
/// rollout. Silent; callers that log do it themselves.
pub async fn signal() {
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
}

/// [`signal`], then the drain log — handed to `axum::serve`'s
/// `with_graceful_shutdown` so in-flight requests finish before exit.
pub async fn drain() {
    signal().await;
    tracing::info!("shutdown signal received; draining connections");
}
