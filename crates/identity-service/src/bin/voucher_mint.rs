//! `voucher-mint` — operator CLI minting registration vouchers (the INSTANT
//! lane of the eligibility slice). Deliberately **not** an HTTP surface: the
//! voucher pool is a Sybil-relevant secret store (threat model 7.2.1), so the
//! only mint path is an operator with database access running this binary.
//!
//! Prints each plaintext key exactly once, to stdout; only sha256 hashes are
//! persisted (threat-model finding M1). A lost key is a dead voucher — there
//! is no recovery path, by design.
//!
//! Usage:
//!   DATABASE_URL=... voucher-mint --count 10 --ttl-days 30 [--batch label]

use anyhow::{bail, Context as _};
use base64::Engine as _;
use rand::RngCore as _;

use identity_service::eligibility;

struct Args {
    count: u32,
    ttl_days: u32,
    batch: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        count: 1,
        ttl_days: 30,
        batch: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .with_context(|| format!("{name} requires a value"))
        };
        match flag.as_str() {
            "--count" => args.count = value("--count")?.parse().context("--count: not a number")?,
            "--ttl-days" => {
                args.ttl_days = value("--ttl-days")?
                    .parse()
                    .context("--ttl-days: not a number")?
            }
            "--batch" => args.batch = Some(value("--batch")?),
            "--help" | "-h" => {
                eprintln!("usage: voucher-mint [--count N] [--ttl-days D] [--batch LABEL]");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    if args.count == 0 || args.ttl_days == 0 {
        bail!("--count and --ttl-days must be at least 1");
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = identity_service::db::connect(&database_url).await?;

    let now = time::OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::days(i64::from(args.ttl_days));
    let batch = args.batch.unwrap_or_else(|| {
        let date = now.date();
        format!("mint-{date}")
    });

    let mut keys = Vec::with_capacity(args.count as usize);
    for _ in 0..args.count {
        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        keys.push(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw));
    }

    // All-or-nothing: a partial mint would leave the operator unsure which
    // printed keys are real.
    let mut tx = pool.begin().await?;
    for key in &keys {
        sqlx::query(
            "INSERT INTO registration_vouchers (key_hash, minted_batch, expires_at) \
             VALUES ($1, $2, $3)",
        )
        .bind(eligibility::key_hash(key))
        .bind(&batch)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // The one and only plaintext emission. stdout carries keys alone so the
    // output can be piped safely; everything else goes to stderr.
    for key in &keys {
        println!("{key}");
    }
    eprintln!(
        "minted {} voucher(s), batch {batch:?}, expires {expires_at}. \
         The keys above are shown ONCE and are not recoverable.",
        keys.len()
    );
    Ok(())
}
