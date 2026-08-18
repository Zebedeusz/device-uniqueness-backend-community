//! `dub` — the single deployable binary for Device Uniqueness Backend.
//!
//! Every service and worker in this workspace is a **role** of this one
//! process: `--role device-attestation-api` is what used to be the `device-attestation-api`
//! binary, byte for byte in behaviour. The role name is also the compose
//! service name, the chart workload name and the Dockerfile stage name — one
//! string end to end, so a typo cannot silently run the wrong thing
//! (`scripts/verify_role_split.sh` checks the agreement).
//!
//! Argument parsing is hand-rolled against `std::env::args`, matching
//! `voucher-mint` — the workspace has no `clap`, and a role selector does not
//! justify introducing one.
//!
//! ```text
//! dub --role device-attestation-api      # become that service
//! dub --list-roles             # the accepted set, one per line
//! dub --healthcheck            # probe this container's own /readyz
//! ```
//!
//! No `--role` (or an unknown one) is a **non-zero exit listing the accepted
//! roles**, never a default: a container that starts as somebody else's
//! service is exactly the failure the per-service image split existed to
//! prevent.

mod healthcheck;

use dub::roles;

use anyhow::{bail, Context as _};

/// What this invocation was asked to do.
enum Command {
    /// Become a role and serve until shut down.
    Run(String),
    /// Print the STANDARD topology's roles, one per line — the source the
    /// deployment gate compares compose services and chart workloads against,
    /// so it can never drift from what this binary actually accepts.
    ListRoles,
    /// Print the SMALL topology's merged roles. Separate from `--list-roles`
    /// because the two topologies are mutually exclusive, and the gate needs to
    /// tell them apart to reject a mix.
    ListMergedRoles,
    /// Probe a `/readyz` and exit 0/1. Defaults to this process's own bind
    /// port on loopback.
    Healthcheck(Option<String>),
}

fn parse_args() -> anyhow::Result<Command> {
    let mut role: Option<String> = None;
    let mut healthcheck = false;
    let mut url: Option<String> = None;

    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .with_context(|| format!("{name} requires a value"))
        };
        match flag.as_str() {
            "--role" => role = Some(value("--role")?),
            "--url" => url = Some(value("--url")?),
            "--healthcheck" => healthcheck = true,
            "--list-roles" => return Ok(Command::ListRoles),
            "--list-merged-roles" => return Ok(Command::ListMergedRoles),
            "--help" | "-h" => {
                eprintln!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\n\n{}", usage()),
        }
    }

    if healthcheck {
        return Ok(Command::Healthcheck(url));
    }
    if url.is_some() {
        bail!("--url is only meaningful with --healthcheck");
    }

    let Some(role) = role else {
        bail!("no --role given\n\n{}", usage());
    };
    if !roles::accepts(&role) {
        bail!("unknown role: {role}\n\n{}", usage());
    }
    Ok(Command::Run(role))
}

fn usage() -> String {
    format!(
        "usage: dub --role <ROLE>\n       dub --list-roles | --list-merged-roles\n       dub --healthcheck [--url URL]\n\nstandard topology (eight workloads):\n  {}\n\nsmall topology (this, plus the three workers above; holds every secret in one\nprocess — see docs/architecture.md, Deployment topologies):\n  {}",
        roles::ROLES.join("\n  "),
        roles::MERGED_ROLES.join("\n  ")
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match parse_args()? {
        Command::ListRoles => {
            for role in roles::ROLES {
                println!("{role}");
            }
            Ok(())
        }
        Command::ListMergedRoles => {
            for role in roles::MERGED_ROLES {
                println!("{role}");
            }
            Ok(())
        }
        Command::Healthcheck(url) => healthcheck::run(url).await,
        Command::Run(role) => roles::run(&role).await,
    }
}
