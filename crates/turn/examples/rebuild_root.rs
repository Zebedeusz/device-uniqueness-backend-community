//! Phase 0 experiment (`docs/plans/done/personhood-proof-turn.md`): rebuild
//! the on-chain ring commitment locally from public storage and byte-compare
//! it with `Members::Root`.
//!
//! `Members::RingKeys` lists every ring member's 32-byte Bandersnatch key in
//! build order (paginated), and `RingKeysStatus.included` says how many of
//! them the current root actually includes. Pushing exactly those keys through
//! the pinned `verifiable` ring builder must reproduce the chain's commitment
//! bit-for-bit — proving our verifier's ring math matches the runtime.
//!
//! ```text
//! cargo run -p turn --example rebuild_root -- \
//!   --rpc wss://previewnet.substrate.dev/people \
//!   --collection 0x<32-byte hex> --ring-index <u32>
//! ```

use anyhow::{bail, Context as _};
use chain_types::people;
use chain_types::people::runtime_types::indiv_support::traits::reality::RingExponent;
use subxt::ext::codec::Decode as _;
use verifiable::ring::ark_vrf::ring::SrsLookup as _;
use verifiable::ring::bandersnatch::{BandersnatchProverCache, BandersnatchVrfVerifiable};
use verifiable::ring::{ProverCache as _, RingDomainSize, StaticChunk};
use verifiable::GenerateVerifiable as _;

type Members = <BandersnatchVrfVerifiable as verifiable::GenerateVerifiable>::Members;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let rpc = arg(&args, "--rpc").context("missing --rpc <url>")?;
    let collection = decode_hex32(&arg(&args, "--collection").context("missing --collection")?)?;
    let ring_index: u32 = arg(&args, "--ring-index")
        .context("missing --ring-index")?
        .parse()
        .context("--ring-index must be a u32")?;

    let api = chain_client::connect(&rpc)
        .await
        .context("connecting to the People Chain RPC")?;
    let at = api.at_current_block().await?;

    let info = at
        .storage()
        .try_fetch(people::storage().members().collections(), (collection,))
        .await?
        .context("collection not found on chain")?
        .decode()?;
    let domain = match info.ring_size {
        RingExponent::R2e9 => RingDomainSize::Domain11,
        RingExponent::R2e10 => RingDomainSize::Domain12,
        RingExponent::R2e14 => RingDomainSize::Domain16,
    };
    println!("ring_size {:?} -> {domain:?}", info.ring_size);

    // RingKeysStatus { total: u32, included: u32, immutable_since: Option<u64> }
    let status_addr = subxt::dynamic::storage::<([u8; 32], u32), subxt::ext::scale_value::Value>(
        "Members",
        "RingKeysStatus",
    );
    let status = at
        .storage()
        .try_fetch(status_addr, (collection, ring_index))
        .await?
        .context("no RingKeysStatus for this ring")?;
    let mut cursor = status.bytes();
    let total = u32::decode(&mut cursor)?;
    let included = u32::decode(&mut cursor)?;
    println!("ring keys: {total} total, {included} included in the current root");

    // RingKeys pages: (collection, ring_index, page) -> BoundedVec<[u8; 32]>
    let mut keys: Vec<[u8; 32]> = Vec::new();
    for page in 0u32.. {
        if keys.len() >= total as usize {
            break;
        }
        let page_addr = subxt::dynamic::storage::<
            ([u8; 32], u32, u32),
            subxt::ext::scale_value::Value,
        >("Members", "RingKeys");
        let Some(value) = at
            .storage()
            .try_fetch(page_addr, (collection, ring_index, page))
            .await?
        else {
            break;
        };
        let page_keys = Vec::<[u8; 32]>::decode(&mut value.bytes())?;
        if page_keys.is_empty() {
            break;
        }
        println!("page {page}: {} keys", page_keys.len());
        keys.extend(page_keys);
    }
    if keys.len() < included as usize {
        bail!(
            "fetched {} keys but the root includes {included}",
            keys.len()
        );
    }
    keys.truncate(included as usize);

    // --old-revision R: find which key-prefix length reproduces the archived
    // OldRoots commitment for revision R (used to build proofs against a
    // specific older revision when testing the accepted-revision window).
    if let Some(rev) = arg(&args, "--old-revision") {
        let rev: u32 = rev.parse().context("--old-revision must be a u32")?;
        let old = at
            .storage()
            .try_fetch(
                subxt::dynamic::storage::<([u8; 32], u32, [u8; 4]), subxt::ext::scale_value::Value>(
                    "Members", "OldRoots",
                ),
                (collection, ring_index, rev.to_be_bytes()),
            )
            .await?
            .context("no OldRoots entry for that revision (expired or never existed)")?;
        let target = Members::decode(&mut old.bytes())
            .map_err(|e| anyhow::anyhow!("old root does not decode: {e}"))?;

        let setup = BandersnatchProverCache::ring_setup(domain);
        let (_, pcs_params) = setup.verifier_key_builder();
        for n in (included.saturating_sub(12)..=included).rev() {
            let mut intermediate = BandersnatchVrfVerifiable::start_members(domain);
            BandersnatchVrfVerifiable::push_members(
                &mut intermediate,
                keys.iter().take(n as usize).copied(),
                |range| {
                    (&pcs_params)
                        .lookup(range)
                        .map(|points| points.into_iter().map(StaticChunk).collect())
                        .ok_or(())
                },
            )
            .map_err(|e| anyhow::anyhow!("push_members failed: {e:?}"))?;
            if BandersnatchVrfVerifiable::finish_members(intermediate) == target {
                println!("MATCH: revision {rev} corresponds to the first {n} keys");
                return Ok(());
            }
        }
        bail!("no prefix in the searched range reproduces revision {rev}");
    }

    // Rebuild the commitment with the pinned verifiable ring builder.
    let setup = BandersnatchProverCache::ring_setup(domain);
    let (_, pcs_params) = setup.verifier_key_builder();
    let mut intermediate = BandersnatchVrfVerifiable::start_members(domain);
    BandersnatchVrfVerifiable::push_members(&mut intermediate, keys.iter().copied(), |range| {
        (&pcs_params)
            .lookup(range)
            .map(|points| points.into_iter().map(StaticChunk).collect())
            .ok_or(())
    })
    .map_err(|e| anyhow::anyhow!("push_members failed: {e:?}"))?;
    let local = BandersnatchVrfVerifiable::finish_members(intermediate);

    // On-chain root (dynamic: value type differs between deployed runtimes).
    let root_addr = subxt::dynamic::storage::<([u8; 32], u32), subxt::ext::scale_value::Value>(
        "Members", "Root",
    );
    let root_value = at
        .storage()
        .try_fetch(root_addr, (collection, ring_index))
        .await?
        .context("ring root not found")?;
    let mut cursor = root_value.bytes();
    let onchain = Members::decode(&mut cursor)
        .map_err(|e| anyhow::anyhow!("on-chain root is not the pinned 288-byte encoding: {e}"))?;
    let revision = u32::decode(&mut cursor)?;

    if local == onchain {
        println!("MATCH: locally rebuilt commitment equals Members::Root (revision {revision})");
        Ok(())
    } else {
        bail!("MISMATCH: local rebuild differs from Members::Root (revision {revision})");
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn decode_hex32(raw: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(raw.trim_start_matches("0x")).context("invalid hex")?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("expected 32 bytes, got {}", v.len()))
}
