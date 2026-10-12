<<<<<<< Updated upstream
// Copyright (C) 2026 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-only

//! Generated People Chain types for the network this build targets.
//!
//! The network is chosen at **build time** by `DUB_NETWORK` (see `build.rs`):
//! `testnet` (the default — paseo-next-v2) or
//! `polkadot` (polkadot-test). Each has its own vendored blob,
//! `metadata/metadata.<network>.scale`. Refresh one with:
//! `subxt metadata --url <people-rpc> --pallets <pallets> -f bytes -o crates/chain-types/metadata/metadata.<network>.scale`
//! where `<pallets>` is `System,Balances,Utility,Proxy,People,PeopleLite,Resources,Members`,
//! plus `Game,ProofOfInk` on `testnet`; the `polkadot` runtime dropped them.
=======
//! Typed People Chain surface, plus the signing configs for the individuality
//! chains the workspace talks to.
//!
//! Static [`subxt`] codegen generated from vendored People Chain metadata. It is
//! the single canonical chain-facing type surface for the workspace: the read
//! path (`Resources::UsernameOwnerOf`, the reservation queue, `PeopleLite::AttestationAllowance`),
//! the device-attestation-chain-writer's registration/attestation calls, and the
//! invite-tickets service's `Game`/`ProofOfInk` invite-ticket calls all build
//! against it.
//! The online transport and signing live in adapter crates, never here.
//!
//! Two subxt configs live here, one per chain family: [`PeopleConfig`] and
//! [`AssetHubConfig`]. Only People is code-generated. The Asset Hub surface the
//! writer needs is `DotnsGateway::reserve_name` plus two storage reads. It is
//! built dynamically by name rather than by vendoring a second, faster-churning
//! metadata blob. See `docs/plans/active/dotns-gateway-write.md`.
//!
//! # Vendored metadata provenance
//!
//! - Source: Paseo Next v2 People Chain — `wss://paseo-people-next-system-rpc.polkadot.io`
//! - Runtime: `next-people-paseo`, specVersion `3004000`, transactionVersion `6`
//! - Format: metadata V16, trimmed to pallets `System`, `Balances`, `Utility`,
//!   `Proxy`, `People`, `PeopleLite`, `Resources`, `Game`, `ProofOfInk`, `Members`
//! - File: [`metadata/people.scale`](../metadata/people.scale)
//!
//! Refresh with:
//! `subxt metadata --url <people-rpc> --pallets System,Balances,Utility,Proxy,People,PeopleLite,Resources,Game,ProofOfInk,Members -f bytes -o crates/chain-types/metadata/people.scale`
>>>>>>> Stashed changes

use subxt::config::transaction_extensions as tx_ext;

/// Generated People Chain runtime API (calls, storage, constants, events).
///
/// Entry points: [`people::storage`], [`people::tx`], [`people::runtime_types`].
#[allow(clippy::all, missing_docs, rustdoc::all)]
#[cfg_attr(
    dub_network = "testnet",
    subxt::subxt(runtime_metadata_path = "metadata/metadata.testnet.scale")
)]
#[cfg_attr(
    dub_network = "polkadot",
    subxt::subxt(runtime_metadata_path = "metadata/metadata.polkadot.scale")
)]
pub mod people {}

pub use subxt;

/// The network this build targets.
pub const NETWORK: &str = env!("DUB_NETWORK");

/// The vendored blob [`people`] is generated from, relative to the workspace root.
pub const METADATA_FILE: &str = concat!(
    "crates/chain-types/metadata/metadata.",
    env!("DUB_NETWORK"),
    ".scale"
);

static METADATA_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/metadata/metadata.",
    env!("DUB_NETWORK"),
    ".scale"
));

/// The vendored metadata [`people`] is generated from, decoded once.
static METADATA: std::sync::LazyLock<subxt::metadata::ArcMetadata> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(
            subxt::Metadata::decode_from(METADATA_BYTES)
                .expect("vendored People Chain metadata decodes"),
        )
    });

/// subxt's own error-decoding entry point
pub fn metadata_arc() -> subxt::metadata::ArcMetadata {
    METADATA.clone()
}

/// Vendored spec_version for people-chain. Asset Hub is dynamically generated
/// so doesn't need this.
pub fn vendored_spec_version() -> u32 {
    static SPEC_VERSION: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        use subxt::ext::scale_decode::DecodeAsType as _;

        let metadata = metadata_arc();
        let version = metadata
            .pallet_by_name("System")
            .and_then(|pallet| pallet.constant_by_name("Version"))
            .expect("vendored metadata carries System::Version");
        people::runtime_types::sp_version::RuntimeVersion::decode_as_type(
            &mut version.value(),
            version.ty(),
            metadata.types(),
        )
        .expect("System::Version decodes as a RuntimeVersion")
        .spec_version
    });
    *SPEC_VERSION
}

macro_rules! delegating_config {
    ($(#[$meta:meta])* $name:ident => $extensions:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default)]
        pub struct $name(subxt::PolkadotConfig);

        impl subxt::Config for $name {
            type AccountId = <subxt::PolkadotConfig as subxt::Config>::AccountId;
            type Address = <subxt::PolkadotConfig as subxt::Config>::Address;
            type Signature = <subxt::PolkadotConfig as subxt::Config>::Signature;
            type Header = <subxt::PolkadotConfig as subxt::Config>::Header;
            type TransactionExtensions = $extensions<Self>;
            type AssetId = <subxt::PolkadotConfig as subxt::Config>::AssetId;
            type Hasher = <subxt::PolkadotConfig as subxt::Config>::Hasher;

            fn genesis_hash(&self) -> Option<subxt::config::HashFor<Self>> {
                self.0.genesis_hash()
            }

            fn spec_and_transaction_version_for_block_number(
                &self,
                block_number: u64,
            ) -> Option<(u32, u32)> {
                self.0
                    .spec_and_transaction_version_for_block_number(block_number)
            }

            fn metadata_for_spec_version(&self, spec_version: u32) -> Option<subxt::ArcMetadata> {
                self.0.metadata_for_spec_version(spec_version)
            }

            fn set_metadata_for_spec_version(
                &self,
                spec_version: u32,
                metadata: subxt::ArcMetadata,
            ) {
                self.0.set_metadata_for_spec_version(spec_version, metadata);
            }
        }
    };
}

macro_rules! extrinsic_params_builder {
    (
        $(#[$meta:meta])*
        $builder:ident<$config:ty> => $extensions:ident,
        |$mortality:ident, $nonce:ident, $tip:ident| $params:expr
    ) => {
        $(#[$meta])*
        pub struct $builder {
            mortality: tx_ext::CheckMortalityParams<$config>,
            nonce: Option<u64>,
            tip: u128,
        }

        impl $builder {
            /// Start with an immortal, no-tip transaction.
            pub fn new() -> Self {
                Self {
                    mortality: tx_ext::CheckMortalityParams::immortal(),
                    nonce: None,
                    tip: 0,
                }
            }

            /// Provide a specific nonce.
            #[must_use]
            pub fn nonce(mut self, nonce: u64) -> Self {
                self.nonce = Some(nonce);
                self
            }

            /// Provide a tip, in the native token.
            #[must_use]
            pub fn tip(mut self, tip: u128) -> Self {
                self.tip = tip;
                self
            }

            /// Provide a mortality window in place of the immortal default.
            #[must_use]
            pub fn mortality(mut self, mortality: tx_ext::CheckMortalityParams<$config>) -> Self {
                self.mortality = mortality;
                self
            }

            /// Build the params tuple for `$extensions`.
            pub fn build(
                self,
            ) -> <$extensions<$config> as subxt::config::TransactionExtensions<$config>>::Params {
                let $mortality = self.mortality;
                let $nonce = self.nonce.map_or_else(
                    tx_ext::CheckNonceParams::from_chain,
                    tx_ext::CheckNonceParams::with_nonce,
                );
                let $tip = tx_ext::ChargeAssetTxPaymentParams::tip(self.tip);
                $params
            }
        }

        impl Default for $builder {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

delegating_config! {
    /// subxt [`Config`](subxt::Config) for People Chain.
    ///
    /// People Chain is Polkadot-shaped (`AccountId32` + `MultiAddress<_, ()>` +
    /// `MultiSignature`, Blake2 hashing), but its transaction-extension pipeline has
    /// People-specific no-op gates before the standard nonce/payment checks.
    ///
    /// The same config serves every People runtime this workspace talks to, even
    /// where they disagree on the extension set: subxt encodes extensions by walking
    /// the **live** runtime's extension list and resolving each name in the tuple, so
    /// a member the connected runtime does not declare is never encoded.
    PeopleConfig => PeopleTransactionExtensions
}

#[rustfmt::skip]
pub type PeopleTransactionExtensions<T> = (
    Noop<UnitTransactionExtension>,
    Noop<AuthorizeValueTransfer>,
    tx_ext::VerifySignature<T>,
    Noop<AsPerson>,
    Noop<AsProofOfInkParticipant>,
    Noop<ScoreAsParticipant>,
    Noop<GameAsInvited>,
    Noop<PeopleLiteAuth>,
    Noop<AsMember>,
    Noop<AsCoinage>,
    Noop<AsResources>,
    Noop<HonourAuth>,
    Noop<AuthorizeCall>,
    Noop<RestrictOrigins>,
    Noop<CheckNonZeroSender>,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<T>,
    tx_ext::CheckMortality<T>,
    tx_ext::CheckNonce,
    Noop<CheckWeight>,
    tx_ext::ChargeAssetTxPayment<T>,
    tx_ext::CheckMetadataHash,
    Noop<StorageWeightReclaim>,
);

extrinsic_params_builder! {
    /// Builder for the People Chain transaction extension params used by the writer.
    PeopleExtrinsicParamsBuilder<PeopleConfig> => PeopleTransactionExtensions,
    |mortality, nonce, tip| (
        (), (), (), (), (), (), (), (), (), (), (), (), (), (), (), (), (), (),
        mortality, nonce, (), tip, (), (),
    )
}

delegating_config! {
    /// subxt [`Config`](subxt::Config) for the individuality Asset Hub runtimes.
    ///
    /// Same Polkadot shape as [`PeopleConfig`]: `AccountId32`,
    /// `MultiAddress<_, ()>`, `MultiSignature`, Blake2. The
    /// transaction-extension tuple is **separate**. Asset Hub declares five gates
    /// People does not (`AsPgas`, `AsRingAlias`, `AsDotnsGateway`,
    /// `PrevalidateAttests`, `EthSetOrigin`). Merging the two sets would exceed
    /// subxt's 26-member tuple ceiling. Keeping them apart also leaves the
    /// proven People pipeline untouched.
    ///
    /// One tuple still serves *both* Asset Hub runtimes. They differ only in the
    /// slot-0 name. That is the same split [`PeopleConfig`] already carries.
    AssetHubConfig => AssetHubTransactionExtensions
}

#[rustfmt::skip]
pub type AssetHubTransactionExtensions<T> = (
    Noop<UnitTransactionExtension>,
    Noop<AuthorizeValueTransfer>,
<<<<<<< Updated upstream
    Noop<VerifyMultiSignature>,
=======
    tx_ext::VerifySignature<T>,
>>>>>>> Stashed changes
    Noop<AuthorizeCall>,
    Noop<AsPgas>,
    Noop<AsRingAlias>,
    Noop<AsScarcity>,
    Noop<AsDotnsGateway>,
    Noop<RestrictOrigins>,
    Noop<CheckNonZeroSender>,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<T>,
    tx_ext::CheckMortality<T>,
    tx_ext::CheckNonce,
    Noop<CheckWeight>,
    tx_ext::ChargeAssetTxPayment<T>,
    Noop<PrevalidateAttests>,
<<<<<<< Updated upstream
=======
    // Real, not a `Noop`. Its *implicit* is `Option<[u8; 32]> = None`. A `Noop`
    // would encode that as nothing, which is a signer-payload mismatch. subxt's
    // version never provides a hash, so it encodes disabled mode.
>>>>>>> Stashed changes
    tx_ext::CheckMetadataHash,
    Noop<EthSetOrigin>,
    Noop<StorageWeightReclaim>,
);

extrinsic_params_builder! {
    /// Builder for the Asset Hub transaction extension params used by the writer.
    AssetHubExtrinsicParamsBuilder<AssetHubConfig> => AssetHubTransactionExtensions,
    |mortality, nonce, tip| (
        (), (), (), (), (), (), (), (), (), (), (), (), (),
        mortality, nonce, (), tip, (), (), (), (),
    )
}

pub trait NoopName {
    const NAME: &'static str;
    const VALUE: &'static [u8] = &[];
}

pub struct Noop<N>(core::marker::PhantomData<fn() -> N>);

impl<N> Clone for Noop<N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N> Copy for Noop<N> {}

impl<N: NoopName> core::fmt::Debug for Noop<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Noop").field(&N::NAME).finish()
    }
}

impl<T: subxt::Config, N: NoopName> subxt::config::TransactionExtension<T> for Noop<N> {
    type Decoded = ();
    type Params = ();

    fn new(
        _client: &subxt::config::ClientState<T>,
        _params: Self::Params,
    ) -> Result<Self, subxt::error::TransactionExtensionError> {
        Ok(Self(core::marker::PhantomData))
    }
}

impl<R: subxt::ext::scale_type_resolver::TypeResolver, N: NoopName>
    subxt::ext::frame_decode::extrinsics::TransactionExtension<R> for Noop<N>
{
    const NAME: &'static str = N::NAME;

    fn encode_value_to(
        &self,
        _type_id: R::TypeId,
        _type_resolver: &R,
        out: &mut Vec<u8>,
    ) -> Result<(), subxt::ext::frame_decode::extrinsics::TransactionExtensionError> {
        out.extend_from_slice(N::VALUE);
        Ok(())
    }

    fn encode_implicit_to(
        &self,
        _type_id: R::TypeId,
        _type_resolver: &R,
        _out: &mut Vec<u8>,
    ) -> Result<(), subxt::ext::frame_decode::extrinsics::TransactionExtensionError> {
        Ok(())
    }
}

macro_rules! noop_names {
    ($($ty:ident $(= $value:expr)?),* $(,)?) => {
        $(
            #[doc = concat!("The `", stringify!($ty), "` transaction extension.")]
            pub struct $ty;

            impl NoopName for $ty {
                const NAME: &'static str = stringify!($ty);
                $(const VALUE: &'static [u8] = $value;)?
            }
        )*
    };
}

// Slot 0 differs per build, not per spec version: Paseo Next v2 declares
// `AuthorizeValueTransfer` (`Option<[u8; 64]>`, encoded `None`), PreviewNet
// declares `UnitTransactionExtension` (`()`). Both names live in the tuple;
// only the one the connected runtime declares is encoded.
noop_names! {
    UnitTransactionExtension,
    AuthorizeValueTransfer = &[0],
    AsPerson = &[0],
    AsProofOfInkParticipant = &[0],
    ScoreAsParticipant = &[0],
    GameAsInvited = &[0],
    PeopleLiteAuth = &[0],
    AsMember = &[0],
    AsCoinage = &[0],
    AsResources = &[0],
    HonourAuth = &[0],
    AuthorizeCall,
    RestrictOrigins = &[0],
    CheckNonZeroSender,
    CheckWeight,
    StorageWeightReclaim,
    // Asset Hub-only origin modifiers. All four are `Option<…>`, encoded
    // `None`. `EthSetOrigin` (pallet_revive's `SetOrigin`) carries no value at all.
    AsPgas = &[0],
    AsRingAlias = &[0],
    AsScarcity = &[0],
    AsDotnsGateway = &[0],
    EthSetOrigin,
<<<<<<< Updated upstream
    VerifyMultiSignature = &[0],
=======
>>>>>>> Stashed changes
    PrevalidateAttests,
}

#[cfg(test)]
mod tests {
    use super::*;
    use subxt::storage::Address as _;

    /// Every extension name [`PeopleTransactionExtensions`](super::PeopleTransactionExtensions)
    /// can satisfy: the `Noop` gates declared above plus the ones subxt implements.
    const TUPLE_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "AuthorizeValueTransfer",
        "VerifyMultiSignature",
        "AsPerson",
        "AsProofOfInkParticipant",
        "ScoreAsParticipant",
        "GameAsInvited",
        "PeopleLiteAuth",
        "AsMember",
        "AsCoinage",
        "AsResources",
        "HonourAuth",
        "AuthorizeCall",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "CheckMetadataHash",
        "StorageWeightReclaim",
    ];

    /// Every extension name [`AssetHubTransactionExtensions`](super::AssetHubTransactionExtensions)
    /// can satisfy. Deliberately a second list rather than a superset of
    /// [`TUPLE_EXTENSIONS`]. The merged set is 30 members and subxt's tuple
    /// impls stop at 26. The two chain families therefore keep separate
    /// pipelines.
    const ASSET_HUB_TUPLE_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "AuthorizeValueTransfer",
        "VerifyMultiSignature",
        "AuthorizeCall",
        "AsPgas",
        "AsRingAlias",
        "AsScarcity",
        "AsDotnsGateway",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "PrevalidateAttests",
        "CheckMetadataHash",
        "EthSetOrigin",
        "StorageWeightReclaim",
    ];

    /// One runtime this workspace knows about: what the chain calls itself,
    /// which runtime build was inspected, and the extension names it declared.
    ///
    /// A human name for diagnostics, plus `spec_name`/`spec_version` to say
    /// which build the list was captured from. `next-people-paseo` numbers two
    /// different deployments. The version alone does not identify a chain.
    struct KnownRuntime {
        /// Human label for diagnostics; never used as a key.
        env: &'static str,
        spec_name: &'static str,
        spec_version: u32,
        /// The tuple that must be able to sign on this runtime.
        tuple: &'static [&'static str],
        /// Ordered transaction-extension identifiers, as declared by that build.
        extensions: &'static [&'static str],
    }

<<<<<<< Updated upstream
    /// What `next-people-paseo` 3000000 declares, in order.
=======
    const PEOPLE_V3004_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "VerifyMultiSignature",
        "AsPerson",
        "AsProofOfInkParticipant",
        "ScoreAsParticipant",
        "GameAsInvited",
        "PeopleLiteAuth",
        "AsMember",
        "AsCoinage",
        "AsResources",
        "HonourAuth",
        "AuthorizeCall",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "CheckMetadataHash",
        "StorageWeightReclaim",
    ];

    /// What `next-people-paseo` 3000000, 3002000 and 3003000 declare, in order.
    /// paseo-next-v2 and previewnet upgraded together through 3002000 and
    /// their metadata is identical, so one list covers both; 3003000 left the
    /// list untouched.
>>>>>>> Stashed changes
    const PEOPLE_V3_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "VerifyMultiSignature",
        "AsPerson",
        "AsProofOfInkParticipant",
        "ScoreAsParticipant",
        "GameAsInvited",
        "PeopleLiteAuth",
        "AsMember",
        "AsCoinage",
        "AsResources",
        "HonourAuth",
        "AuthorizeCall",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "StorageWeightReclaim",
    ];

    /// What `people-polkadot` 2005000 declares, in order (extension version 1,
    /// the one subxt encodes with). Game and ProofOfInk are gone, and with
    /// them their gates; `CheckMetadataHash` is new.
    const PEOPLE_POLKADOT_2005000_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "VerifyMultiSignature",
        "AsPerson",
        "PeopleLiteAuth",
        "AsMember",
        "AsCoinage",
        "AsResources",
        "RestrictOrigins",
        "AuthorizeCall",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "CheckMetadataHash",
        "StorageWeightReclaim",
    ];

    /// What `next-asset-hub-paseo` 3000000 declares, in order. `AsRingAlias`
    /// is gone and `AsScarcity` took its place; the tuple carries both,
    /// because the runtimes below are still in the list.
    const ASSET_HUB_V3_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "AsScarcity",
        "AuthorizeCall",
        "AsPgas",
        "AsDotnsGateway",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "CheckMetadataHash",
        "EthSetOrigin",
        "StorageWeightReclaim",
    ];

<<<<<<< Updated upstream
    /// What `statemint` 2005000 (the polkadot-test Asset Hub) declares, in
    /// order (extension version 1). New against the Paseo Asset Hubs:
    /// `VerifyMultiSignature` and `PrevalidateAttests`; `AsScarcity` is gone.
    const ASSET_HUB_POLKADOT_2005000_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "VerifyMultiSignature",
=======
    const ASSET_HUB_V3004_EXTENSIONS: &[&str] = &[
        "UnitTransactionExtension",
        "VerifyMultiSignature",
        "AsScarcity",
>>>>>>> Stashed changes
        "AuthorizeCall",
        "AsPgas",
        "AsDotnsGateway",
        "RestrictOrigins",
        "CheckNonZeroSender",
        "CheckSpecVersion",
        "CheckTxVersion",
        "CheckGenesis",
        "CheckMortality",
        "CheckNonce",
        "CheckWeight",
        "ChargeAssetTxPayment",
        "PrevalidateAttests",
        "CheckMetadataHash",
        "EthSetOrigin",
        "StorageWeightReclaim",
    ];

    /// Every runtime a deployment is known to have talked to. The tuples are
    /// the union of these sets, not a snapshot of the newest one: an entry
    /// stays here — and its gate stays in the tuple — so that a binary
    /// pointed at a node that has not upgraded yet can still sign.
    const KNOWN_RUNTIMES: &[KnownRuntime] = &[
        KnownRuntime {
            env: "polkadot-test",
            spec_name: "people-polkadot",
            spec_version: 2_005_000,
            tuple: TUPLE_EXTENSIONS,
            extensions: PEOPLE_POLKADOT_2005000_EXTENSIONS,
        },
        KnownRuntime {
            env: "polkadot-test asset hub",
            spec_name: "statemint",
            spec_version: 2_005_000,
            tuple: ASSET_HUB_TUPLE_EXTENSIONS,
            extensions: ASSET_HUB_POLKADOT_2005000_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2",
            spec_name: "next-people-paseo",
            spec_version: 3_004_000,
            tuple: TUPLE_EXTENSIONS,
            extensions: PEOPLE_V3004_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2 / previewnet asset hub",
            spec_name: "next-asset-hub-paseo",
            spec_version: 3_004_000,
            tuple: ASSET_HUB_TUPLE_EXTENSIONS,
            extensions: ASSET_HUB_V3004_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2",
            spec_name: "next-people-paseo",
            spec_version: 3_003_000,
            tuple: TUPLE_EXTENSIONS,
            extensions: PEOPLE_V3_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2 / previewnet",
            spec_name: "next-people-paseo",
            spec_version: 3_002_000,
            tuple: TUPLE_EXTENSIONS,
            extensions: PEOPLE_V3_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2 / previewnet",
            spec_name: "next-people-paseo",
            spec_version: 3_000_000,
            tuple: TUPLE_EXTENSIONS,
            extensions: PEOPLE_V3_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2 asset hub",
            spec_name: "next-asset-hub-paseo",
            spec_version: 3_000_000,
            tuple: ASSET_HUB_TUPLE_EXTENSIONS,
            extensions: ASSET_HUB_V3_EXTENSIONS,
        },
        KnownRuntime {
            env: "paseo-next-v2",
            spec_name: "next-people-paseo",
            spec_version: 1_000_030,
            tuple: TUPLE_EXTENSIONS,
            extensions: &[
                "AuthorizeValueTransfer",
                "VerifyMultiSignature",
                "AsPerson",
                "AsProofOfInkParticipant",
                "ScoreAsParticipant",
                "GameAsInvited",
                "PeopleLiteAuth",
                "AsMember",
                "AsCoinage",
                "AsResources",
                "HonourAuth",
                "AuthorizeCall",
                "RestrictOrigins",
                "CheckNonZeroSender",
                "CheckSpecVersion",
                "CheckTxVersion",
                "CheckGenesis",
                "CheckMortality",
                "CheckNonce",
                "CheckWeight",
                "ChargeAssetTxPayment",
                "StorageWeightReclaim",
            ],
        },
        KnownRuntime {
            env: "paseo-next-v2 asset hub",
            spec_name: "next-asset-hub-paseo",
            spec_version: 2_000_033,
            tuple: ASSET_HUB_TUPLE_EXTENSIONS,
            extensions: &[
                "AuthorizeValueTransfer",
                "AuthorizeCall",
                "AsPgas",
                "AsRingAlias",
                "AsDotnsGateway",
                "RestrictOrigins",
                "CheckNonZeroSender",
                "CheckSpecVersion",
                "CheckTxVersion",
                "CheckGenesis",
                "CheckMortality",
                "CheckNonce",
                "CheckWeight",
                "ChargeAssetTxPayment",
                "CheckMetadataHash",
                "EthSetOrigin",
                "StorageWeightReclaim",
            ],
        },
    ];

    /// One binary signs on either runtime of a family. subxt resolves
    /// extensions by name against the connected runtime's list. The tuple must
    /// therefore satisfy every name each known build declares. A runtime that
    /// adds a name (e.g. `CheckMetadataHash` on public Paseo) fails here first.
    ///
    /// This is a snapshot, not a live check: it holds the tuple to what was
    /// observed, and cannot notice a runtime that has since been upgraded.
    #[test]
    fn covers_every_known_runtime_extension_set() {
        for runtime in KNOWN_RUNTIMES {
            for name in runtime.extensions {
                assert!(
                    runtime.tuple.contains(name),
                    "{} ({} {}) declares extension {name}, which the tuple cannot satisfy",
                    runtime.env,
                    runtime.spec_name,
                    runtime.spec_version
                );
            }
        }
    }

    /// Every extension the vendored metadata declares must have a slot in
    /// [`PeopleTransactionExtensions`](super::PeopleTransactionExtensions).
    /// [`KNOWN_RUNTIMES`] is a hand-captured snapshot; this reads the list out
    /// of the metadata the crate generates against, so a metadata refresh that
    /// introduces an extension fails here without waiting for a new capture.
    #[test]
    fn vendored_metadata_extensions_are_all_in_the_tuple() {
        let metadata = metadata_arc();
        let declared: Vec<&str> = metadata
            .extrinsic()
            .transaction_extensions_to_use_for_encoding()
            .map(|extension| extension.identifier())
            .collect();

        assert!(
            !declared.is_empty(),
            "vendored metadata must declare transaction extensions"
        );
        for name in &declared {
            assert!(
                TUPLE_EXTENSIONS.contains(name),
                "vendored metadata declares extension {name}, which the tuple cannot satisfy"
            );
        }
    }

    #[test]
    fn a_v4_extrinsic_resolves_to_extension_version_zero() {
        assert_eq!(
            metadata_arc()
                .extrinsic()
                .transaction_extension_version_to_use_for_decoding(),
            0,
            "subxt must read a versionless (v4) extrinsic as extension \
             version 0; anything else signs bytes the runtime cannot decode"
        );
    }

    #[test]
    fn version_zero_extensions_are_all_in_the_tuple() {
        let metadata = metadata_arc();
        let declared: Vec<&str> = metadata
            .extrinsic()
            .transaction_extensions_by_version(0)
            .expect("version 0 is always declared")
            .map(|extension| extension.identifier())
            .collect();

        assert!(
            !declared.is_empty(),
            "version 0 must declare transaction extensions"
        );
        for name in &declared {
            assert!(
                TUPLE_EXTENSIONS.contains(name),
                "version 0 declares extension {name}, which the tuple cannot \
                 satisfy — a v4 extrinsic would not encode it"
            );
        }
    }

    /// The two slot-0 spellings encode their own value shapes: Paseo Next v2's
    /// `Option<[u8; 64]>` as `None`, PreviewNet's `()` as nothing at all.
    #[test]
    fn vendored_metadata_names_the_runtime_it_came_from() {
        assert_eq!(
            vendored_spec_version(),
<<<<<<< Updated upstream
            VENDORED_VERSION.0,
=======
            3_004_000,
>>>>>>> Stashed changes
            "the blob's own System::Version is what chain-client logs the live \
             chain against, so refreshing the blob moves this number with it"
        );
    }

    #[test]
    fn slot_zero_noops_encode_their_runtime_value() {
        assert_eq!(AuthorizeValueTransfer::NAME, "AuthorizeValueTransfer");
        assert_eq!(AuthorizeValueTransfer::VALUE, &[0]);
        assert_eq!(UnitTransactionExtension::NAME, "UnitTransactionExtension");
        assert!(UnitTransactionExtension::VALUE.is_empty());
    }

    /// The read path can construct the `Resources::UsernameOwnerOf` lookup used
    /// for on-chain username availability. In subxt 0.50 the map key is supplied
    /// at fetch time, so the address itself is keyless but typed on the key.
    #[test]
    fn builds_username_owner_of_query() {
        let query = people::storage().resources().username_owner_of();
        assert_eq!(query.pallet_name(), "Resources");
        assert_eq!(query.entry_name(), "UsernameOwnerOf");
    }

    /// The resource watch can construct the `PeopleLite::AttestationAllowance`
    /// lookup (keyed by attester at fetch time).
    #[test]
    fn builds_attestation_allowance_query() {
        let query = people::storage().people_lite().attestation_allowance();
        assert_eq!(query.pallet_name(), "PeopleLite");
        assert_eq!(query.entry_name(), "AttestationAllowance");
    }

    /// The device-attestation-chain-writer can construct the `Resources::register_lite_person`
    /// registration call from the generated API.
    #[test]
    fn builds_register_lite_person_call() {
        let username =
            people::runtime_types::bounded_collections::bounded_vec::BoundedVec(b"alice".to_vec());
        let call = people::tx()
            .resources()
            .register_lite_person([0u8; 65], username, None);
        assert_eq!(call.pallet_name(), "Resources");
        assert_eq!(call.call_name(), "register_lite_person");
    }

<<<<<<< Updated upstream
    #[cfg(invite_tickets)]
=======
    /// The invite-tickets pool can construct both DIM variants of `set_invite_ticket`.
>>>>>>> Stashed changes
    #[test]
    fn builds_set_invite_ticket_calls() {
        let ticket = subxt::utils::AccountId32([0u8; 32]);
        let game = people::tx().game().set_invite_ticket(ticket);
        assert_eq!(game.pallet_name(), "Game");
        assert_eq!(game.call_name(), "set_invite_ticket");

        let poi = people::tx().proof_of_ink().set_invite_ticket(ticket);
        assert_eq!(poi.pallet_name(), "ProofOfInk");
        assert_eq!(poi.call_name(), "set_invite_ticket");
    }

<<<<<<< Updated upstream
    #[cfg(invite_tickets)]
=======
    /// The invite-tickets pool can construct both DIM variants of the
    /// `AvailableInvites` quota lookup (keyed by inviter at fetch time).
>>>>>>> Stashed changes
    #[test]
    fn builds_available_invites_queries() {
        let game = people::storage().game().available_invites();
        assert_eq!(game.pallet_name(), "Game");
        assert_eq!(game.entry_name(), "AvailableInvites");

        let poi = people::storage().proof_of_ink().available_invites();
        assert_eq!(poi.pallet_name(), "ProofOfInk");
        assert_eq!(poi.entry_name(), "AvailableInvites");
    }

<<<<<<< Updated upstream
    #[cfg(dub_network = "polkadot")]
    const VENDORED_VERSION: (u32, u32) = (2_005_000, 0);
    #[cfg(not(dub_network = "polkadot"))]
    const VENDORED_VERSION: (u32, u32) = (3_000_000, 5);

=======
    /// The vendored metadata, decoded the same way subxt would receive it from
    /// a node, so extension construction and encoding can run offline.
>>>>>>> Stashed changes
    fn people_metadata() -> subxt::ArcMetadata {
        subxt::Metadata::decode_from(METADATA_BYTES)
            .expect("vendored metadata decodes")
            .arc()
    }

    /// An offline stand-in for the client state subxt hands to transaction
    /// extensions, using the versions the crate docs record for the vendored
    /// runtime.
    fn offline_client_state() -> subxt::config::ClientState<PeopleConfig> {
        subxt::config::ClientState {
            genesis_hash: subxt::utils::H256::zero(),
<<<<<<< Updated upstream
            spec_version: VENDORED_VERSION.0,
            transaction_version: VENDORED_VERSION.1,
=======
            spec_version: 3_004_000,
            transaction_version: 6,
>>>>>>> Stashed changes
            metadata: people_metadata(),
        }
    }

    /// `.nonce(n)` must reach the wire. The params tuple hides the nonce behind
    /// a private field, so the `CheckNonce` slot is run through subxt's own
    /// offline constructor and encoder: an explicit nonce comes out as its
    /// SCALE compact encoding, while the `new()` default encodes none of its
    /// own and defers to the nonce subxt injects from the chain.
    #[test]
    fn builder_nonce_flows_into_check_nonce_encoding() {
        let state = offline_client_state();

        fn encode_nonce(
            state: &subxt::config::ClientState<PeopleConfig>,
            params: tx_ext::CheckNonceParams,
        ) -> Vec<u8> {
            use subxt::config::TransactionExtension as _;
            use subxt::ext::frame_decode::extrinsics::TransactionExtension as _;

            let mut out = Vec::new();
            tx_ext::CheckNonce::new(state, params)
                .expect("CheckNonce builds offline")
                .encode_value_to(0, state.metadata.types(), &mut out)
                .expect("CheckNonce encodes offline");
            out
        }

        let (.., nonce, _, _, _, _) = PeopleExtrinsicParamsBuilder::new().nonce(7).build();
        assert_eq!(
            encode_nonce(&state, nonce),
            [28],
            "nonce 7 must encode as Compact(7), 7 << 2"
        );

        let (.., nonce, _, _, _, _) = PeopleExtrinsicParamsBuilder::new().build();
        assert_eq!(
            encode_nonce(&state, nonce),
            [0],
            "without an explicit nonce the slot carries none of its own and \
             defers to the nonce subxt injects from the chain"
        );
    }

    /// `new()` promises an immortal, no-tip transaction: the mortality slot
    /// must encode the immortal era byte and the payment slot must carry a
    /// zero tip in the native token, both checked through subxt's own
    /// constructors rather than the builder's inputs.
    #[test]
    fn builder_starts_immortal_with_no_tip() {
        use subxt::config::transaction_extensions::{ChargeAssetTxPayment, CheckMortality};
        use subxt::config::TransactionExtension as _;
        use subxt::ext::frame_decode::extrinsics::TransactionExtension as _;

        let state = offline_client_state();
        let (.., mortality, _, _, tip, _, _) = PeopleExtrinsicParamsBuilder::new().build();

        let mut era = Vec::new();
        CheckMortality::new(&state, mortality)
            .expect("immortal params build offline")
            .encode_value_to(0, state.metadata.types(), &mut era)
            .expect("CheckMortality encodes offline");
        assert_eq!(era, [0], "immortal era is the single zero byte");

        let payment = ChargeAssetTxPayment::new(&state, tip).expect("tip params build offline");
        assert_eq!(payment.tip(), 0, "the builder must not tip by default");
        assert!(payment.asset_id().is_none(), "tip is in the native token");
    }

    /// `Default` must stay in lockstep with `new()`: the mortality, nonce and
    /// payment slots of both builders have to encode to identical bytes,
    /// checked through subxt's own constructors rather than the builders'
    /// inputs.
    #[test]
    fn default_builder_matches_new() {
        use subxt::config::TransactionExtension as _;
        use subxt::ext::frame_decode::extrinsics::TransactionExtension as _;

        let state = offline_client_state();

        let mut encoded = Vec::new();
        for builder in [
            PeopleExtrinsicParamsBuilder::new(),
            PeopleExtrinsicParamsBuilder::default(),
        ] {
            let (.., mortality, nonce, _, tip, _, _) = builder.build();
            let mut out = Vec::new();

            tx_ext::CheckMortality::new(&state, mortality)
                .expect("mortality params build offline")
                .encode_value_to(0, state.metadata.types(), &mut out)
                .expect("CheckMortality encodes offline");
            tx_ext::CheckNonce::new(&state, nonce)
                .expect("CheckNonce builds offline")
                .encode_value_to(0, state.metadata.types(), &mut out)
                .expect("CheckNonce encodes offline");
            tx_ext::ChargeAssetTxPayment::new(&state, tip)
                .expect("tip params build offline")
                .encode_value_to(0, state.metadata.types(), &mut out)
                .expect("ChargeAssetTxPayment encodes offline");

            encoded.push(out);
        }

        assert_eq!(
            encoded[0], encoded[1],
            "`default()` must encode identically to `new()`"
        );
    }

    /// The `Noop` encoder impl is the code path that actually writes a gate's
    /// bytes into an extrinsic: `encode_value_to` must append exactly the
    /// declared value (the encoded-`None` byte for `AuthorizeValueTransfer`,
    /// nothing for the unit-valued `UnitTransactionExtension`) and
    /// `encode_implicit_to` must write nothing, whatever the buffer holds.
    #[test]
    fn noop_encoders_write_value_bytes_and_empty_implicit() {
        use subxt::config::TransactionExtension as _;
        use subxt::ext::frame_decode::extrinsics::TransactionExtension as _;

        let state = offline_client_state();
        let types = state.metadata.types();

        let authorize =
            Noop::<AuthorizeValueTransfer>::new(&state, ()).expect("noop gates build offline");
        let mut out = vec![0xAA];
        authorize
            .encode_value_to(0, types, &mut out)
            .expect("noop value encodes");
        assert_eq!(out, [0xAA, 0x00], "appends exactly the encoded-None byte");
        authorize
            .encode_implicit_to(0, types, &mut out)
            .expect("noop implicit encodes");
        assert_eq!(out, [0xAA, 0x00], "the implicit part writes nothing");

        let unit =
            Noop::<UnitTransactionExtension>::new(&state, ()).expect("noop gates build offline");
        let mut out = vec![0xAA];
        unit.encode_value_to(0, types, &mut out)
            .expect("noop value encodes");
        unit.encode_implicit_to(0, types, &mut out)
            .expect("noop implicit encodes");
        assert_eq!(out, [0xAA], "a unit-valued gate writes no bytes at all");
    }

    /// The `Config` impl must hand PolkadotConfig's caches through unchanged:
    /// a fresh offline config knows nothing (no genesis hash, no spec-version
    /// ranges, no metadata), and a metadata registration is readable back
    /// through the same wrapper as the same `Arc`, not a rebuild.
    #[test]
    fn config_delegates_caches_to_polkadot_config() {
        use subxt::Config as _;

        let config = PeopleConfig::default();
        assert_eq!(config.genesis_hash(), None);
        assert_eq!(
            config.spec_and_transaction_version_for_block_number(0),
            None
        );
        assert!(config.metadata_for_spec_version(3_000_000).is_none());

        let metadata = people_metadata();
        config.set_metadata_for_spec_version(3_000_000, metadata.clone());
        let cached = config
            .metadata_for_spec_version(3_000_000)
            .expect("registered metadata is readable back");
        assert!(
            std::sync::Arc::ptr_eq(&cached, &metadata),
            "delegation must return the registered Arc"
        );
        assert!(
            config.metadata_for_spec_version(3_000_001).is_none(),
            "registration must not leak to other spec versions"
        );
    }

    /// The personhood-proof verifier can construct the `Members` pallet lookups
    /// it verifies ring-VRF proofs against: the current ring root, the
    /// retained previous roots, and the collection descriptor carrying the
    /// ring size (`Root`/`OldRoots` keyed by collection at fetch time).
    #[test]
    fn builds_members_ring_root_queries() {
        let root = people::storage().members().root();
        assert_eq!(root.pallet_name(), "Members");
        assert_eq!(root.entry_name(), "Root");

        let old_roots = people::storage().members().old_roots();
        assert_eq!(old_roots.pallet_name(), "Members");
        assert_eq!(old_roots.entry_name(), "OldRoots");

        let collections = people::storage().members().collections();
        assert_eq!(collections.pallet_name(), "Members");
        assert_eq!(collections.entry_name(), "Collections");
    }
}
