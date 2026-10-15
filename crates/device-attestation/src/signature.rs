// Copyright (C) 2026 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-only

//! Account signatures over messages that are not transactions.
//!
//! Individuality runtimes above spec version 3004000 check these signatures
//! against the personhood signing context followed by the message, and reject
//! the plain message. Earlier runtimes check the plain message only. The backend
//! reads the spec version of the chain that verifies a signature and applies the
//! same rule, so one release works before and after the runtime upgrade.
//!
//! The signing context is `blake2_256("product/peopl." ++ network_suffix ++
//! "/" ++ "sig/" ++ 28 zero bytes)`, as the runtime's
//! `indiv_support::context::signing_payload` builds it for the personhood
//! product, which owns the people-lite and dotNS gateway signatures. The network
//! suffix is the chain's `NetworkSuffix` storage value.

use anyhow::Context as _;
use subxt::config::{substrate::BlakeTwo256, Hasher as _};
use subxt::OnlineClient;
use subxt_signer::sr25519::{verify, PublicKey, Signature};

/// The pallet that holds the network suffix. Only individuality runtimes have
/// it.
const NETWORK_SUFFIX_PALLET: &str = "NetworkSuffix";

/// The last spec version that checks account signatures over the plain message.
/// Delete the plain form once every served network runs a later release.
const LAST_PLAIN_SPEC_VERSION: u32 = 3_004_000;

/// The message form a runtime accepts for an account signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSignatureForm {
    /// The message alone.
    Plain,
    /// The 32-byte signing context followed by the message.
    InContext([u8; 32]),
}

/// Reads the message form `client`'s runtime accepts for personhood account
/// signatures at the current block.
pub(crate) async fn account_signature_form<T: subxt::Config>(
    client: &OnlineClient<T>,
) -> anyhow::Result<AccountSignatureForm> {
    let at = client.at_current_block().await?;
    let has_network_suffix = at
        .metadata_ref()
        .pallet_by_name(NETWORK_SUFFIX_PALLET)
        .is_some();
    if !uses_signing_context(at.spec_version(), has_network_suffix) {
        return Ok(AccountSignatureForm::Plain);
    }
    let address = subxt::dynamic::storage::<(), Vec<u8>>(NETWORK_SUFFIX_PALLET, "NetworkSuffix");
    let suffix = at
        .storage()
        .fetch(address, ())
        .await
        .context("reading NetworkSuffix::NetworkSuffix")?
        .decode()?;
    Ok(AccountSignatureForm::InContext(signing_context(&suffix)))
}

/// Whether a runtime checks account signatures in the signing context.
///
/// `has_network_suffix` tells an individuality runtime apart from another
/// chain whose spec version happens to be higher.
fn uses_signing_context(spec_version: u32, has_network_suffix: bool) -> bool {
    has_network_suffix && spec_version > LAST_PLAIN_SPEC_VERSION
}

/// Builds the personhood signing context on the network with `network_suffix`.
pub(crate) fn signing_context(network_suffix: &[u8]) -> [u8; 32] {
    BlakeTwo256
        .hash(&[b"product/peopl.", network_suffix, b"/sig/", &[0u8; 28]].concat())
        .0
}

/// Verifies that `signer`'s sr25519 key signed `message` in `form`.
///
/// Returns `false` for a signature that is not 64 bytes.
pub(crate) fn verify_account_signature(
    signature: &[u8],
    message: &[u8],
    signer: &[u8; 32],
    form: &AccountSignatureForm,
) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let context: &[u8] = match form {
        AccountSignatureForm::Plain => &[],
        AccountSignatureForm::InContext(context) => context,
    };
    verify(
        &Signature(signature),
        [context, message].concat(),
        &PublicKey(*signer),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;
    use subxt_signer::sr25519::Keypair;
    use subxt_signer::SecretUri;

    fn alice() -> Keypair {
        Keypair::from_uri(&SecretUri::from_str("//Alice").unwrap()).unwrap()
    }

    /// The runtime's personhood signing context on a `paseo` network, from its
    /// `indiv_support::context` test.
    const PERSONHOOD_PASEO: &str =
        "50a0143eac8dc44969947d41ad201c0717c0456425b2631fefdcfbd7b2b23d62";

    #[test]
    fn signing_context_matches_the_runtime() {
        assert_eq!(hex::encode(signing_context(b"paseo")), PERSONHOOD_PASEO);
    }

    #[test]
    fn signing_context_starts_above_the_last_plain_spec_version() {
        assert!(!uses_signing_context(3_004_000, true));
        assert!(uses_signing_context(3_004_001, true));
        // A chain without the network suffix pallet is not an individuality
        // runtime, whatever its spec version.
        assert!(!uses_signing_context(3_004_001, false));
    }

    #[test]
    fn each_form_accepts_only_its_own_message() {
        let keypair = alice();
        let signer = keypair.public_key().0;
        let context = signing_context(b"paseo");
        let in_context_form = AccountSignatureForm::InContext(context);
        let message = b"message";

        let plain = keypair.sign(message).0;
        let in_context = keypair.sign(&[context.as_slice(), message].concat()).0;

        assert!(verify_account_signature(
            &plain,
            message,
            &signer,
            &AccountSignatureForm::Plain
        ));
        assert!(!verify_account_signature(
            &in_context,
            message,
            &signer,
            &AccountSignatureForm::Plain
        ));
        assert!(verify_account_signature(
            &in_context,
            message,
            &signer,
            &in_context_form
        ));
        assert!(!verify_account_signature(
            &plain,
            message,
            &signer,
            &in_context_form
        ));
    }

    #[test]
    fn rejects_other_contexts_and_short_signatures() {
        let keypair = alice();
        let signer = keypair.public_key().0;
        let form = AccountSignatureForm::InContext(signing_context(b"paseo"));
        let message = b"message";

        // A signature in another network's context does not verify.
        let other_network = signing_context(b"testnet");
        let foreign = keypair
            .sign(&[other_network.as_slice(), message].concat())
            .0;
        assert!(!verify_account_signature(&foreign, message, &signer, &form));

        // A signature that is not 64 bytes is rejected, not panicked on.
        assert!(!verify_account_signature(
            &[0u8; 10], message, &signer, &form
        ));
    }
}
