//! Live coverage for the batched availability read.
//!
//! `ChainClient::taken_discriminators` asks the node about `{base}.00` ..
//! `{base}.99` in ONE `state_queryStorageAt` request. These tests prove it
//! agrees with the typed one-key-per-request path against real chain state;
//! response-shape validation is covered offline in `chain::client`.
//!
//! Read-only. Every chain interaction is under a timeout, so an unreachable
//! endpoint fails with a message instead of hanging on the reconnecting
//! client's retry loop.

use std::collections::BTreeSet;
use std::future::Future;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use chain_types::{people, PeopleConfig};
use device_attestation::chain::ChainClient;

/// Discriminators per base (`00`..`99`), mirroring the production constant.
const DISCRIMINATORS: u8 = 100;

/// Budget for one chain interaction: generous, because previewnet shares a host
/// with Paseo, but finite, because the reconnecting client retries forever.
const STEP_TIMEOUT: Duration = Duration::from_secs(90);

fn rpc_url() -> String {
    std::env::var("PEOPLE_RPC_URL")
        .unwrap_or_else(|_| "wss://previewnet.substrate.dev/people".to_string())
}

/// Await `future` under [`STEP_TIMEOUT`], naming `what` and the URL on expiry.
async fn step<T>(what: &str, future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(STEP_TIMEOUT, future).await {
        Ok(value) => value,
        Err(_) => panic!(
            "{what} exceeded {}s against {} — endpoint unreachable or overloaded",
            STEP_TIMEOUT.as_secs(),
            rpc_url()
        ),
    }
}

type PeopleBlock = subxt::client::ClientAtBlock<
    PeopleConfig,
    subxt::client::OnlineClientAtBlockImpl<PeopleConfig>,
>;

fn owner_key(
    username: impl AsRef<[u8]>,
) -> people::runtime_types::bounded_collections::bounded_vec::BoundedVec<u8> {
    people::runtime_types::bounded_collections::bounded_vec::BoundedVec(username.as_ref().to_vec())
}

/// The pre-batching path — typed storage, one request per key — as the
/// reference. Takes the block the batched read uses, so both see one snapshot.
async fn taken_discriminators_per_key(
    at: &PeopleBlock,
    base: &str,
) -> anyhow::Result<BTreeSet<u8>> {
    let storage = at.storage();
    let storage = &storage;

    let lookups = (0..DISCRIMINATORS).map(move |discriminator| async move {
        let username = format!("{base}.{discriminator:02}");
        let owned = storage
            .try_fetch(
                people::storage().resources().username_owner_of(),
                (owner_key(username),),
            )
            .await?
            .is_some();
        anyhow::Ok((discriminator, owned))
    });

    Ok(futures::future::try_join_all(lookups)
        .await?
        .into_iter()
        .filter_map(|(discriminator, owned)| owned.then_some(discriminator))
        .collect())
}

/// Find a registered lite username on this chain, as `(base, discriminator)`.
///
/// Only `{base}.{NN}` entries count: any other suffix says nothing about the
/// `00`..`99` range, so accepting one would let the comparison pass against
/// empty state.
async fn some_registered_lite_username(at: &PeopleBlock) -> anyhow::Result<Option<(String, u8)>> {
    let entry = at
        .storage()
        .entry(people::storage().resources().username_owner_of())?;
    let mut entries = entry.iter(()).await?;

    while let Some(item) = entries.next().await {
        let (username,) = item?.key()?.decode()?;
        let username = String::from_utf8(username.0)?;
        let Some((base, suffix)) = username.rsplit_once('.') else {
            continue;
        };
        if suffix.len() != 2 || !suffix.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if base.is_empty() {
            continue;
        }
        return Ok(Some((base.to_string(), suffix.parse()?)));
    }
    Ok(None)
}

#[tokio::test]
#[ignore = "requires a live People Chain RPC; set PEOPLE_RPC_URL and run with --ignored"]
async fn batched_read_matches_per_key_read() {
    let chain = step("connect", ChainClient::connect(&rpc_url()))
        .await
        .expect("connect");
    let at = step("selecting a block", chain.online().at_current_block())
        .await
        .expect("current block");

    let (owned_base, owned_discriminator) =
        step("scanning usernames", some_registered_lite_username(&at))
            .await
            .expect("iterate usernames")
            .expect(
                "no registered {base}.{NN} username on this chain: the comparison would only \
                 prove two empty sets agree — point PEOPLE_RPC_URL at a chain with registrations",
            );
    let free_base = format!("dubprobe{}", std::process::id());

    for base in [owned_base.clone(), free_base.clone()] {
        let per_key = step(
            "per-key read (100 requests)",
            taken_discriminators_per_key(&at, &base),
        )
        .await
        .expect("per-key read");
        let batched = step(
            "batched read (1 request)",
            chain.taken_discriminators_at(&base, &at),
        )
        .await
        .expect("batched read");

        assert_eq!(
            per_key, batched,
            "batched read disagreed with the per-key read for base {base}"
        );

        if base == owned_base {
            assert!(
                batched.contains(&owned_discriminator),
                "expected {owned_base}.{owned_discriminator:02} to read as taken; \
                 got {batched:?} — the comparison proved nothing"
            );
        } else {
            assert!(
                batched.is_empty(),
                "unregistered probe base {free_base} read as taken: {batched:?}"
            );
        }
    }
}

/// `min / p50 / p95 / max` of a set of per-request durations, in ms.
fn percentiles(mut samples: Vec<u128>) -> (u128, u128, u128, u128) {
    samples.sort_unstable();
    let pick = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    (
        samples[0],
        pick(0.50),
        pick(0.95),
        samples[samples.len() - 1],
    )
}

/// Diagnostic, not a correctness test: prints both paths' latency under
/// concurrent callers and asserts nothing, since network timings are not a
/// stable assertion. It shows what one caller cannot — subxt fans its 100
/// requests out concurrently and never contends with itself.
///
/// Opt in with `BATCH_PROBE_CONCURRENCY=<n>`, so the live-chain suite stays a
/// correctness gate. Keep `n` modest: previewnet shares a host with Paseo.
#[tokio::test]
#[ignore = "diagnostic probe; set PEOPLE_RPC_URL + BATCH_PROBE_CONCURRENCY and run with --ignored"]
async fn concurrent_load_probe_prints_latency() {
    let Some(raw) = std::env::var("BATCH_PROBE_CONCURRENCY").ok() else {
        eprintln!("BATCH_PROBE_CONCURRENCY unset: skipping the latency probe");
        return;
    };
    let concurrency: NonZeroUsize = raw
        .parse()
        .expect("BATCH_PROBE_CONCURRENCY must be a positive integer");
    let concurrency = concurrency.get();

    let chain = step("connect", ChainClient::connect(&rpc_url()))
        .await
        .expect("connect");
    let online = chain.online().clone();

    let bases: Vec<String> = (0..concurrency)
        .map(|i| format!("dubload{}x{i}", std::process::id()))
        .collect();

    // Warm-up round (discarded): the first request on a fresh connection pays
    // for metadata and subscription setup on both paths.
    let at = step("selecting a block", online.at_current_block())
        .await
        .expect("current block");
    let _ = taken_discriminators_per_key(&at, &bases[0]).await;
    let _ = chain.taken_discriminators(&bases[0]).await;

    for round in 1..=2 {
        let wall = Instant::now();
        let per_key: Vec<u128> = futures::future::join_all(bases.iter().map(|base| async {
            let started = Instant::now();
            let at = online.at_current_block().await.expect("current block");
            taken_discriminators_per_key(&at, base)
                .await
                .expect("per-key read");
            started.elapsed().as_millis()
        }))
        .await;
        let per_key_wall = wall.elapsed().as_millis();

        let wall = Instant::now();
        let batched: Vec<u128> = futures::future::join_all(bases.iter().map(|base| {
            let chain = chain.clone();
            async move {
                let started = Instant::now();
                chain
                    .taken_discriminators(base)
                    .await
                    .expect("batched read");
                started.elapsed().as_millis()
            }
        }))
        .await;
        let batched_wall = wall.elapsed().as_millis();

        let (a_min, a_p50, a_p95, a_max) = percentiles(per_key);
        let (b_min, b_p50, b_p95, b_max) = percentiles(batched);
        eprintln!(
            "round {round} @ concurrency {concurrency}\n  \
             100 requests each ({} total): min {a_min} p50 {a_p50} p95 {a_p95} max {a_max} ms | wall {per_key_wall} ms\n  \
             1 request each ({concurrency} total):  min {b_min} p50 {b_p50} p95 {b_p95} max {b_max} ms | wall {batched_wall} ms",
            concurrency * 100
        );
    }
}
