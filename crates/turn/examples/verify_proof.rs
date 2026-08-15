//! Phase 0 of `docs/plans/done/personhood-proof-turn.md`: read the People
//! Chain `Members` ring root and collection info at the latest finalized
//! block, and (when given one) verify a real host-produced proof against it,
//! printing the 32-byte contextual alias.
//!
//! ```text
//! cargo run -p turn --example verify_proof -- \
//!   --rpc wss://paseo-people-next-system-rpc.polkadot.io \
//!   --collection 0x<32-byte hex> --ring-index <u32> --domain 11|12|16 \
//!   [--proof-file proof.scale --context 0x<hex> --message 0x<hex>]
//! ```
//!
//! Without `--proof-file` it stops after printing the root and ring size —
//! useful for checking whether the on-chain commitment encoding matches the
//! pinned `verifiable` revision (288-byte `MembersCommitment`) before a real
//! proof fixture exists.

use anyhow::{bail, Context as _};
use chain_types::people;
use subxt::ext::codec::Decode as _;
use verifiable::ring::bandersnatch::BandersnatchVrfVerifiable;
use verifiable::ring::RingDomainSize;
use verifiable::GenerateVerifiable as _;

type Members = <BandersnatchVrfVerifiable as verifiable::GenerateVerifiable>::Members;
type Proof = <BandersnatchVrfVerifiable as verifiable::GenerateVerifiable>::Proof;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let rpc = arg(&args, "--rpc").context("missing --rpc <url>")?;
    let collection = decode_hex32(&arg(&args, "--collection").context("missing --collection")?)?;
    let ring_index: u32 = arg(&args, "--ring-index")
        .context("missing --ring-index")?
        .parse()
        .context("--ring-index must be a u32")?;
    let domain = match arg(&args, "--domain").as_deref() {
        Some("11") => RingDomainSize::Domain11,
        Some("12") => RingDomainSize::Domain12,
        Some("16") => RingDomainSize::Domain16,
        other => bail!("--domain must be 11, 12, or 16 (got {other:?})"),
    };

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
    println!("collection ring_size: {:?}", info.ring_size);

    // `Members::Root`'s value type differs between deployed runtimes (the
    // 768-byte pre-#1163 encoding vs the pinned 288-byte one), so a static
    // address from vendored metadata cannot match both — fetch dynamically
    // and decode the `RingRoot { root, revision, .. }` prefix by hand.
    let root_addr = subxt::dynamic::storage::<([u8; 32], u32), subxt::ext::scale_value::Value>(
        "Members", "Root",
    );
    let root_value = at
        .storage()
        .try_fetch(root_addr, (collection, ring_index))
        .await?
        .context("ring root not found for this collection + ring index")?;
    let root_bytes = root_value.bytes();
    println!("ring root value: {} encoded bytes total", root_bytes.len());

    let mut cursor = root_bytes;
    let commitment = match Members::decode(&mut cursor) {
        Ok(commitment) => commitment,
        Err(e) => bail!(
            "on-chain root does not decode as the pinned verifiable \
             MembersCommitment (expects the 288-byte encoding): {e}. The \
             deployed runtime likely predates individuality#1163 — see the \
             plan's Phase 0 finding.",
        ),
    };
    let revision = u32::decode(&mut cursor).context("decoding root revision")?;
    println!("ring root: revision {revision}, commitment decoded OK (288-byte encoding)");

    let Some(proof_path) = arg(&args, "--proof-file") else {
        println!("no --proof-file given; root decoded OK, stopping here");
        return Ok(());
    };
    let proof_bytes = std::fs::read(&proof_path).context("reading --proof-file")?;
    let proof = Proof::try_from(proof_bytes)
        .map_err(|_| anyhow::anyhow!("proof file is longer than a ring-VRF signature"))?;
    let ctx = decode_hex(&arg(&args, "--context").context("missing --context")?)?;
    let message = decode_hex(&arg(&args, "--message").context("missing --message")?)?;

    match BandersnatchVrfVerifiable::validate(domain, &proof, &commitment, &ctx, &message) {
        Ok(alias) => println!("alias: 0x{}", hex::encode(alias)),
        Err(e) => bail!("proof did NOT verify: {e:?}"),
    }
    Ok(())
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn decode_hex(raw: &str) -> anyhow::Result<Vec<u8>> {
    hex::decode(raw.trim_start_matches("0x")).context("invalid hex")
}

fn decode_hex32(raw: &str) -> anyhow::Result<[u8; 32]> {
    decode_hex(raw)?
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("expected 32 bytes, got {}", v.len()))
}
