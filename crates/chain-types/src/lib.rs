//! Typed People Chain surface for the workspace's services.
//!
//! Static [`subxt`] codegen generated from vendored People Chain metadata. It is
//! the single canonical chain-facing type surface for the workspace: the read
//! path (`Resources::UsernameOwnerOf`, the reservation queue, `PeopleLite::AttestationAllowance`),
//! the identity-chain-writer's registration/attestation calls, and the dim-tickets
//! service's `Game`/`ProofOfInk` invite-ticket calls all build against it.
//! The online transport and signing live in adapter crates, never here.
//!
//! # Vendored metadata provenance
//!
//! - Source: Paseo Next v2 People Chain — `wss://paseo-people-next-system-rpc.polkadot.io`
//! - Runtime: `next-people-paseo`, specVersion `1000029`, transactionVersion `3`
//! - Format: metadata V16, trimmed to pallets `System`, `Balances`, `Utility`,
//!   `Proxy`, `People`, `PeopleLite`, `Resources`, `Game`, `ProofOfInk`
//! - File: [`metadata/people.scale`](../metadata/people.scale)
//!
//! Refresh with:
//! `subxt metadata --url <people-rpc> --pallets System,Balances,Utility,Proxy,People,PeopleLite,Resources,Game,ProofOfInk -f bytes -o crates/chain-types/metadata/people.scale`

/// Generated People Chain runtime API (calls, storage, constants, events).
///
/// Entry points: [`people::storage`], [`people::tx`], [`people::runtime_types`].
#[allow(clippy::all, missing_docs, rustdoc::all)]
#[subxt::subxt(runtime_metadata_path = "metadata/people.scale")]
pub mod people {}

pub use subxt;

/// The vendored metadata [`people`] is generated from, decoded once.
///
/// Same bytes as the codegen, so pallet and error indices agree with the typed
/// surface. Lets a service resolve a runtime `DispatchError` to its
/// `Pallet::Variant` names without a chain round-trip.
pub fn metadata() -> &'static subxt::Metadata {
    static METADATA: std::sync::LazyLock<subxt::Metadata> = std::sync::LazyLock::new(|| {
        subxt::Metadata::decode_from(include_bytes!("../metadata/people.scale"))
            .expect("vendored People Chain metadata decodes")
    });
    &METADATA
}

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
#[derive(Debug, Clone, Default)]
pub struct PeopleConfig(subxt::PolkadotConfig);

impl subxt::Config for PeopleConfig {
    type AccountId = <subxt::PolkadotConfig as subxt::Config>::AccountId;
    type Address = <subxt::PolkadotConfig as subxt::Config>::Address;
    type Signature = <subxt::PolkadotConfig as subxt::Config>::Signature;
    type Header = <subxt::PolkadotConfig as subxt::Config>::Header;
    type TransactionExtensions = PeopleTransactionExtensions<Self>;
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

    fn set_metadata_for_spec_version(&self, spec_version: u32, metadata: subxt::ArcMetadata) {
        self.0.set_metadata_for_spec_version(spec_version, metadata);
    }
}

type PeopleTransactionExtensions<T> = (
    Noop<UnitTransactionExtension>,
    Noop<AuthorizeValueTransfer>,
    subxt::config::transaction_extensions::VerifySignature<T>,
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
    subxt::config::transaction_extensions::CheckSpecVersion,
    subxt::config::transaction_extensions::CheckTxVersion,
    subxt::config::transaction_extensions::CheckGenesis<T>,
    subxt::config::transaction_extensions::CheckMortality<T>,
    subxt::config::transaction_extensions::CheckNonce,
    Noop<CheckWeight>,
    subxt::config::transaction_extensions::ChargeAssetTxPayment<T>,
    Noop<StorageWeightReclaim>,
);

/// Builder for the People Chain transaction extension params used by the writer.
pub struct PeopleExtrinsicParamsBuilder<T: subxt::Config> {
    mortality: subxt::config::transaction_extensions::CheckMortalityParams<T>,
    nonce: Option<u64>,
    tip: u128,
}

impl<T: subxt::Config> PeopleExtrinsicParamsBuilder<T> {
    /// Start with an immortal, no-tip transaction.
    pub fn new() -> Self {
        Self {
            mortality: subxt::config::transaction_extensions::CheckMortalityParams::immortal(),
            nonce: None,
            tip: 0,
        }
    }

    /// Provide a specific nonce.
    pub fn nonce(mut self, nonce: u64) -> Self {
        self.nonce = Some(nonce);
        self
    }

    /// Build params for [`PeopleTransactionExtensions`].
    pub fn build(
        self,
    ) -> <PeopleTransactionExtensions<T> as subxt::config::TransactionExtensions<T>>::Params {
        (
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            (),
            self.mortality,
            self.nonce.map_or_else(
                subxt::config::transaction_extensions::CheckNonceParams::from_chain,
                subxt::config::transaction_extensions::CheckNonceParams::with_nonce,
            ),
            (),
            subxt::config::transaction_extensions::ChargeAssetTxPaymentParams::tip(self.tip),
            (),
        )
    }
}

impl<T: subxt::Config> Default for PeopleExtrinsicParamsBuilder<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub trait NoopName {
    const NAME: &'static str;
    const VALUE: &'static [u8] = &[];
}

#[derive(Debug, Clone)]
pub struct Noop<N>(core::marker::PhantomData<N>);

impl<T: subxt::Config, N: NoopName + Send + Sync + 'static> subxt::config::TransactionExtension<T>
    for Noop<N>
{
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
    const NAME: &str = N::NAME;

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

macro_rules! noop_name {
    ($ty:ident, $name:literal) => {
        pub struct $ty;
        impl NoopName for $ty {
            const NAME: &'static str = $name;
        }
    };
    ($ty:ident, $name:literal, $value:expr) => {
        pub struct $ty;
        impl NoopName for $ty {
            const NAME: &'static str = $name;
            const VALUE: &'static [u8] = $value;
        }
    };
}

// Slot 0 of the People extension pipeline is named differently per runtime:
// `next-people-paseo` 1000029 (Paseo Next v2) calls it `AuthorizeValueTransfer`
// with an `Option<[u8; 64]>` value (encoded `None`), while 1000030 (PreviewNet)
// calls it `UnitTransactionExtension` with a `()` value. Both names live in the
// tuple; only the one the connected runtime declares is ever encoded.
noop_name!(UnitTransactionExtension, "UnitTransactionExtension");
noop_name!(AuthorizeValueTransfer, "AuthorizeValueTransfer", &[0]);
noop_name!(AsPerson, "AsPerson", &[0]);
noop_name!(AsProofOfInkParticipant, "AsProofOfInkParticipant", &[0]);
noop_name!(ScoreAsParticipant, "ScoreAsParticipant", &[0]);
noop_name!(GameAsInvited, "GameAsInvited", &[0]);
noop_name!(PeopleLiteAuth, "PeopleLiteAuth", &[0]);
noop_name!(AsMember, "AsMember", &[0]);
noop_name!(AsCoinage, "AsCoinage", &[0]);
noop_name!(AsResources, "AsResources", &[0]);
noop_name!(HonourAuth, "HonourAuth", &[0]);
noop_name!(AuthorizeCall, "AuthorizeCall");
noop_name!(RestrictOrigins, "RestrictOrigins", &[0]);
noop_name!(CheckNonZeroSender, "CheckNonZeroSender");
noop_name!(CheckWeight, "CheckWeight");
noop_name!(StorageWeightReclaim, "StorageWeightReclaim");

#[cfg(test)]
mod tests {
    use super::people;
    use super::NoopName as _;
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
        "StorageWeightReclaim",
    ];

    /// One People runtime this workspace knows about: what the chain calls
    /// itself, which runtime build was inspected, and the extension names it
    /// declared.
    ///
    /// A human name for diagnostics plus `spec_name`/`spec_version` to say which
    /// build the list was captured from — `next-people-paseo` numbers two
    /// different deployments, so the version alone does not identify a chain.
    struct KnownRuntime {
        /// Human label for diagnostics; never used as a key.
        env: &'static str,
        spec_name: &'static str,
        spec_version: u32,
        /// Ordered transaction-extension identifiers, as declared by that build.
        extensions: &'static [&'static str],
    }

    /// The People runtimes this workspace is verified against, captured
    /// 2026-08-01. Not exhaustive — they differ in slot 0 only.
    const KNOWN_RUNTIMES: &[KnownRuntime] = &[
        KnownRuntime {
            env: "paseo-next-v2",
            spec_name: "next-people-paseo",
            spec_version: 1_000_029,
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
            env: "previewnet",
            spec_name: "next-people-paseo",
            spec_version: 1_000_030,
            extensions: &[
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
            ],
        },
    ];

    /// One binary signs on either People runtime: subxt resolves extensions by
    /// name against the connected runtime's list, so the tuple must be able to
    /// satisfy every name each known build declares. A runtime that adds a
    /// name (e.g. `CheckMetadataHash` on public Paseo) fails here first.
    ///
    /// This is a snapshot, not a live check: it holds the tuple to what was
    /// observed, and cannot notice a runtime that has since been upgraded.
    #[test]
    fn covers_every_known_runtime_extension_set() {
        for runtime in KNOWN_RUNTIMES {
            for name in runtime.extensions {
                assert!(
                    TUPLE_EXTENSIONS.contains(name),
                    "{} ({} {}) declares extension {name}, which the tuple cannot satisfy",
                    runtime.env,
                    runtime.spec_name,
                    runtime.spec_version
                );
            }
        }
    }

    /// The two slot-0 spellings encode their own value shapes: Paseo Next v2's
    /// `Option<[u8; 64]>` as `None`, PreviewNet's `()` as nothing at all.
    #[test]
    fn slot_zero_noops_encode_their_runtime_value() {
        assert_eq!(
            super::AuthorizeValueTransfer::NAME,
            "AuthorizeValueTransfer"
        );
        assert_eq!(super::AuthorizeValueTransfer::VALUE, &[0]);
        assert_eq!(
            super::UnitTransactionExtension::NAME,
            "UnitTransactionExtension"
        );
        assert!(super::UnitTransactionExtension::VALUE.is_empty());
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

    /// The identity-chain-writer can construct the `Resources::register_lite_person`
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

    /// The dim-tickets writer can construct both DIM variants of `set_invite_ticket`.
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

    /// The dim-tickets API can construct both DIM variants of the
    /// `AvailableInvites` quota lookup (keyed by inviter at fetch time).
    #[test]
    fn builds_available_invites_queries() {
        use subxt::storage::Address as _;

        let game = people::storage().game().available_invites();
        assert_eq!(game.pallet_name(), "Game");
        assert_eq!(game.entry_name(), "AvailableInvites");

        let poi = people::storage().proof_of_ink().available_invites();
        assert_eq!(poi.pallet_name(), "ProofOfInk");
        assert_eq!(poi.entry_name(), "AvailableInvites");
    }

    /// The vendored metadata, decoded the same way subxt would receive it from
    /// a node, so extension construction and encoding can run offline.
    fn people_metadata() -> subxt::ArcMetadata {
        subxt::Metadata::decode_from(include_bytes!("../metadata/people.scale"))
            .expect("vendored metadata decodes")
            .arc()
    }

    /// An offline stand-in for the client state subxt hands to transaction
    /// extensions, using the versions the crate docs record for the vendored
    /// runtime.
    fn offline_client_state() -> subxt::config::ClientState<super::PeopleConfig> {
        subxt::config::ClientState {
            genesis_hash: subxt::utils::H256::zero(),
            spec_version: 1_000_029,
            transaction_version: 3,
            metadata: people_metadata(),
        }
    }

    /// `.nonce(n)` must reach the wire. The params tuple hides the nonce behind
    /// a private field, so the `CheckNonce` slot is run through subxt's own
    /// offline constructor and encoder: an explicit nonce comes out as its
    /// SCALE compact encoding, while the `new()` default stays fetch-from-chain
    /// (`None`), observable offline only via the params' `Debug` shape.
    #[test]
    fn builder_nonce_flows_into_check_nonce_encoding() {
        use subxt::config::transaction_extensions::CheckNonce;
        use subxt::config::TransactionExtension as _;
        use subxt::ext::frame_decode::extrinsics::TransactionExtension as _;

        let state = offline_client_state();

        let (.., nonce, _, _, _) =
            super::PeopleExtrinsicParamsBuilder::<super::PeopleConfig>::new()
                .nonce(7)
                .build();
        assert_eq!(format!("{nonce:?}"), "CheckNonceParams(Some(7))");
        let mut encoded = Vec::new();
        CheckNonce::new(&state, nonce)
            .expect("CheckNonce builds offline")
            .encode_value_to(0, state.metadata.types(), &mut encoded)
            .expect("CheckNonce encodes offline");
        assert_eq!(encoded, [28], "nonce 7 must encode as Compact(7), 7 << 2");

        let (.., nonce, _, _, _) =
            super::PeopleExtrinsicParamsBuilder::<super::PeopleConfig>::new().build();
        assert_eq!(
            format!("{nonce:?}"),
            "CheckNonceParams(None)",
            "without an explicit nonce the slot must defer to the chain lookup"
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
        let (.., mortality, _, _, tip, _) =
            super::PeopleExtrinsicParamsBuilder::<super::PeopleConfig>::new().build();

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

    /// `Default` must stay in lockstep with `new()`: same fetch-from-chain
    /// nonce, same zero-tip payment params.
    #[test]
    fn default_builder_matches_new() {
        let (.., new_nonce, _, new_tip, _) =
            super::PeopleExtrinsicParamsBuilder::<super::PeopleConfig>::new().build();
        let (.., default_nonce, _, default_tip, _) =
            super::PeopleExtrinsicParamsBuilder::<super::PeopleConfig>::default().build();

        assert_eq!(format!("{default_nonce:?}"), format!("{new_nonce:?}"));
        assert_eq!(format!("{default_tip:?}"), format!("{new_tip:?}"));
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

        let authorize = super::Noop::<super::AuthorizeValueTransfer>::new(&state, ())
            .expect("noop gates build offline");
        let mut out = vec![0xAA];
        authorize
            .encode_value_to(0, types, &mut out)
            .expect("noop value encodes");
        assert_eq!(out, [0xAA, 0x00], "appends exactly the encoded-None byte");
        authorize
            .encode_implicit_to(0, types, &mut out)
            .expect("noop implicit encodes");
        assert_eq!(out, [0xAA, 0x00], "the implicit part writes nothing");

        let unit = super::Noop::<super::UnitTransactionExtension>::new(&state, ())
            .expect("noop gates build offline");
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

        let config = super::PeopleConfig::default();
        assert_eq!(config.genesis_hash(), None);
        assert_eq!(
            config.spec_and_transaction_version_for_block_number(0),
            None
        );
        assert!(config.metadata_for_spec_version(1_000_029).is_none());

        let metadata = people_metadata();
        config.set_metadata_for_spec_version(1_000_029, metadata.clone());
        let cached = config
            .metadata_for_spec_version(1_000_029)
            .expect("registered metadata is readable back");
        assert!(
            std::sync::Arc::ptr_eq(&cached, &metadata),
            "delegation must return the registered Arc"
        );
        assert!(
            config.metadata_for_spec_version(1_000_030).is_none(),
            "registration must not leak to other spec versions"
        );
    }
}
