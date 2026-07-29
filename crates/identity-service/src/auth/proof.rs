//! sr25519 client-proof verification.
//!
//! The client signs `SHA256(challenge || clientId || SHA256(body))` with its
//! sr25519 account key. `clientId` is the raw 32-byte public key; the resulting
//! `accountId` is its `0x`-hex encoding. Verification uses `subxt_signer`, which
//! is schnorrkel with the Substrate signing context — symmetric with the real
//! Polkadot wallets in the shipping apps.

use sha2::{Digest as _, Sha256};
use subxt_signer::sr25519::{PublicKey, Signature};

/// Recompute the signed message: `SHA256(challenge || clientId || SHA256(body))`.
///
/// Shared construction: the sr25519 proof signs exactly this hash, and the iOS
/// App Attest assertion uses it as the `clientDataHash` (the shipping client's
/// `AppAttestClientHashCalculator` builds the same bytes).
pub(crate) fn client_data_hash(challenge: &[u8], client_id: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let body_hash = Sha256::digest(body);
    let mut hasher = Sha256::new();
    hasher.update(challenge);
    hasher.update(client_id);
    hasher.update(body_hash);
    hasher.finalize().into()
}

/// Verify the sr25519 proof over the challenge, client id, and request body.
pub fn verify(challenge: &[u8], client_id: &[u8; 32], body: &[u8], proof: &[u8; 64]) -> bool {
    let message = client_data_hash(challenge, client_id, body);
    subxt_signer::sr25519::verify(&Signature(*proof), message, &PublicKey(*client_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;
    use subxt_signer::sr25519::Keypair;
    use subxt_signer::SecretUri;

    #[test]
    fn accepts_shipping_mobile_body_variants() {
        let keypair = Keypair::from_uri(&SecretUri::from_str("//Alice").unwrap()).unwrap();
        let client_id = keypair.public_key().0;
        let challenge = b"challenge-bytes";
        let ios_and_play_integrity_body = b"{}";

        let message = client_data_hash(challenge, &client_id, ios_and_play_integrity_body);
        let proof = keypair.sign(&message).0;

        assert!(verify(
            challenge,
            &client_id,
            ios_and_play_integrity_body,
            &proof
        ));
        // Wrong body invalidates the proof (body is bound into the hash).
        assert!(!verify(challenge, &client_id, b"tampered", &proof));

        let key_attestation_body = b"{\"attestationChain\":[\"certificate\"]}";
        let message = client_data_hash(challenge, &client_id, key_attestation_body);
        let proof = keypair.sign(&message).0;
        assert!(verify(challenge, &client_id, key_attestation_body, &proof));
    }
}
