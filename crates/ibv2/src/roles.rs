//! The deployable roles, one module per former binary.
//!
//! Each module holds that binary's `main` body unchanged, minus the
//! `#[tokio::main]` attribute (the runtime is started once, in `main`) and the
//! per-binary copy of the shutdown signal handler (now [`crate::shutdown`]).
//!
//! Two properties are load-bearing and must survive any edit here:
//!
//! - **Every role installs its own telemetry and metrics with its own service
//!   name**, as the first two lines of `run`. Those are process-global and
//!   `metrics::spawn` sets `service` as a global label, so the name must be
//!   the role's — hoisting either call into `main` would relabel every
//!   scrape target and dashboard.
//! - **Every role builds only its own config.** `Config::from_env` is
//!   fail-fast by design, so constructing another role's config here would
//!   make this role require that role's environment.

pub mod all_in_one;
pub mod dim_tickets_api;
pub mod dim_tickets_writer;
pub mod identity_api;
pub mod identity_chain_writer;
pub mod invite_tickets_api;
pub mod invite_tickets_pool;
pub mod notify_relay;
pub mod registration_queue;
pub mod turn_api;
pub mod username_indexer;

/// Every role this binary accepts, in deployment order (the six HTTP services,
/// then the four single-instance workers).
///
/// This slice is the authority: `--list-roles` prints it, and the deployment
/// gate compares the compose services and chart workloads against that output
/// rather than against a second hand-written list.
pub const ROLES: &[&str] = &[
    "identity-api",
    "username-indexer",
    "dim-tickets-api",
    "invite-tickets-api",
    "turn-api",
    "notify-relay",
    "identity-chain-writer",
    "registration-queue",
    "dim-tickets-writer",
    "invite-tickets-pool",
];

/// The roles that make up the **small** topology's HTTP tier: one merged
/// process instead of the six per-service ones.
///
/// Deliberately absent from [`ROLES`]: a deployment renders one topology or the
/// other, never a mix, and `scripts/verify_role_split.sh` enforces that by
/// treating these two lists as mutually exclusive sets. Selecting this one also
/// requires an explicit acknowledgement in the values file, because it collapses
/// the secret compartmentalisation the standard topology enforces — see
/// "Deployment topologies" in `docs/architecture.md`.
pub const MERGED_ROLES: &[&str] = &["all-in-one"];

/// The four single-instance workers, which belong to **both** topologies: each
/// owns a Postgres lease and a nonce lane, so they are never merged into
/// anything.
pub const WORKER_ROLES: &[&str] = &[
    "identity-chain-writer",
    "registration-queue",
    "dim-tickets-writer",
    "invite-tickets-pool",
];

/// Every role `--role` accepts, in either topology.
pub fn accepts(role: &str) -> bool {
    ROLES.contains(&role) || MERGED_ROLES.contains(&role)
}

/// Become `role`. The caller has already checked it with [`accepts`].
pub async fn run(role: &str) -> anyhow::Result<()> {
    match role {
        "all-in-one" => all_in_one::run().await,
        "identity-api" => identity_api::run().await,
        "username-indexer" => username_indexer::run().await,
        "dim-tickets-api" => dim_tickets_api::run().await,
        "invite-tickets-api" => invite_tickets_api::run().await,
        "turn-api" => turn_api::run().await,
        "notify-relay" => notify_relay::run().await,
        "identity-chain-writer" => identity_chain_writer::run().await,
        "registration-queue" => registration_queue::run().await,
        "dim-tickets-writer" => dim_tickets_writer::run().await,
        "invite-tickets-pool" => invite_tickets_pool::run().await,
        // Unreachable via `main`, which validates against ROLES first. Kept
        // exhaustive rather than `unreachable!` so adding a name to ROLES
        // without a dispatch arm is a compile error, not a runtime one.
        other => anyhow::bail!("role {other} is listed but not dispatched"),
    }
}

#[cfg(test)]
mod tests {
    use super::ROLES;

    /// The gate compares deployment manifests against `--list-roles`, so a
    /// duplicate would let two workloads legitimately claim one role.
    #[test]
    fn roles_are_unique() {
        let mut sorted = ROLES.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate role in ROLES");
    }

    /// Ten deployables, unchanged by this refactor: six HTTP services and four
    /// single-instance workers. A change here is a deployment change and must
    /// move the chart, compose and the gate in the same commit.
    #[test]
    fn ten_deployable_roles() {
        assert_eq!(ROLES.len(), 10, "ROLES: {ROLES:?}");
    }

    /// The two topologies are disjoint sets of HTTP roles. The gate compares
    /// a deployment's roles against them, so an overlap here would let a values
    /// file run the merged process AND the per-service ones — paying the cost of
    /// the secret union while still operating six compartmentalised services.
    #[test]
    fn the_topologies_are_disjoint() {
        assert!(super::accepts("all-in-one"));
        for role in super::MERGED_ROLES {
            assert!(!ROLES.contains(role), "{role} is in both topologies");
        }
    }

    /// The workers belong to both topologies, so they must be deployable roles
    /// in their own right — the small topology is `all-in-one` PLUS these four,
    /// never `all-in-one` alone.
    #[test]
    fn the_workers_are_deployable_in_both_topologies() {
        assert_eq!(super::WORKER_ROLES.len(), 4);
        for role in super::WORKER_ROLES {
            assert!(ROLES.contains(role), "{role} is not a deployable role");
        }
        for role in super::MERGED_ROLES {
            assert!(!super::WORKER_ROLES.contains(role));
        }
    }
}
