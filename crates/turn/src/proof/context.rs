//! Product proof contexts, derived exactly as the hosts derive them.
//!
//! A host proves under `blake2b256("product/" ++ product_id ++ "/" ++
//! derivation_index_bytes(suffix))`. That is the only context shape TrUAPI can
//! express: `createAccountProof` takes a `ProductProofContext { productId,
//! suffix }`, and RFC-0024 states there is no well-known-context namespace and
//! no second context scheme. Deriving one context per accepted product — rather
//! than pinning a single shared one — keeps every product proving under its own
//! identifier.
//!
//! The alias is `alias_in_context(secret, context)`, so a per-product context
//! also makes the throttle key per person *and* product, with no cross-product
//! linkability.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest as _};

/// Blake2b truncated to 256 bits, matching the hosts' `blake2b256`.
type Blake2b256 = Blake2b<U32>;

/// Separates plain-index space from raw 32-byte derivation indexes:
/// `blake2b256("product-account-index")[..28]`.
fn index_magic() -> [u8; 28] {
    let digest: [u8; 32] = Blake2b256::digest(b"product-account-index").into();
    let mut magic = [0u8; 28];
    magic.copy_from_slice(&digest[..28]);
    magic
}

/// The 32-byte derivation index for a plain `u32` suffix: the index
/// little-endian followed by the index magic.
pub fn index_bytes(index: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&index.to_le_bytes());
    bytes[4..].copy_from_slice(&index_magic());
    bytes
}

/// The context a host derives for `product_id` at derivation index `suffix`.
pub fn product_context(product_id: &str, suffix: u32) -> [u8; 32] {
    let suffix = index_bytes(suffix);
    let mut hasher = Blake2b256::new();
    hasher.update(b"product/");
    hasher.update(product_id.as_bytes());
    hasher.update(b"/");
    hasher.update(suffix);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_carries_the_little_endian_index_then_the_magic() {
        let bytes = index_bytes(7);
        assert_eq!(&bytes[..4], &7u32.to_le_bytes());
        assert_eq!(&bytes[4..], &index_magic());
        assert_ne!(index_bytes(7), index_bytes(8));
    }

    #[test]
    fn the_magic_matches_the_hosts_constant() {
        // `blake2b256("product-account-index")[..28]`, the value both truapi
        // and the Nova hosts derive their product accounts under.
        assert_eq!(
            hex::encode(index_magic()),
            "12e86013736c5498f050b03cdc16957dff0e422fb92ca77ec3ab168f"
        );
    }

    #[test]
    fn matches_truapi_and_individuality_known_answers() {
        // Pinned in Individuality PR #1300 / truapi `context_bytes_pins_derivation_index_preimage`
        assert_eq!(
            hex::encode(product_context("voting.dot", 0)),
            "fc8e5a62a2abf020f4f5bc5d00c06c18404674804c8dacd5198357c5c761440d"
        );
    }
}
