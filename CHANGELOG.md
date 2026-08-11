# Changelog

Notable changes to the published `ghcr.io/paritytech/identity-backend-rust`
image. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0, a breaking change bumps the **minor**. External consumers should pin an
exact `vX.Y.Z`; `stable` tracks the newest release and `latest` tracks `main`
(unreleased work — see [README](README.md#releases)).

Entries at or before 0.1.2 predate this file and are reconstructed from history:
they summarise each release rather than list every change.

## [Unreleased]

### Added

- dotNS write lane through the gateway, enabled by default ([#31], [#33]).
- Prometheus metrics for the attester's on-chain resources, exported from the
  existing `/metrics` route ([#26]).
- PreviewNet environment on its own hostname, running a deliberate subset of the
  services ([#23]).
- Device attestation across App Attest, Play Integrity, Key Attestation and
  DeviceCheck ([#15]).
- Registration queue, and a proof-of-compute gate on public username search
  (`/api/v1/poc` through the gateway) ([#16]).

### Changed

- **Breaking:** `INVITER_PROXY_FOR` and `INVITE_INVITER_PROXY_FOR` are removed.
  `dim-tickets-writer` and `invite-tickets-pool` now derive proxy signing the way
  the chain writer already did in [#32]: the signing key's own account is
  compared with `INVITER_ADDRESS`, and the batch is wrapped in
  `Proxy.proxy(real = INVITER_ADDRESS)` when they differ. Delete the variables
  from deployment env files — they are no longer read, and a value that used to
  disagree with `INVITER_ADDRESS` (previously a startup abort) now silently
  proxies for `INVITER_ADDRESS` instead ([#46]).
- **Breaking:** the attester account is named once by `ATTESTER_ACCOUNT`,
  replacing the per-service variables ([#32]).
- The registration queue is now the default behaviour rather than opt-in
  ([#24]).
- `/api/v1/usernames/search` omits entries whose identifier key carries a
  non-zero type byte (pre-RFC-0004 P-256 keys) ([#22]).

### Fixed

- `invite-tickets-pool` submits ticket batches again after the Paseo Next v2
  wipe: the vendored People metadata is recaptured at spec `1000030`, whose
  `Game` calls and `Utility.force_batch` shape subxt validates payloads against.
- Deploy creates the `ibv2-metrics` network before starting services, instead of
  failing on a host that never had it ([#29]).
- `username-indexer` fails fast when the proof-of-compute gate is enabled with no
  JWT material configured.

## [0.1.2] - 2026-07-27

First release published as a multi-arch (amd64 + arm64) image.

### Added

- Tagged multi-arch release images built on native per-arch runners, kept
  independent of the `main` deploy lane ([#12], [#13], [#14]).
- `notify-relay`, the stateless APNs / FCM push relay ([#9]).
- `turn-api`, the stateless coturn REST-API credential issuer ([#7]).
- DIM and invitation-ticket services ([#1], [#5]).
- API reference generated from the services' `utoipa` annotations ([#4]).
- Single-URL gateway in front of every service ([#8]).

### Changed

- **Breaking** (usernames rewrite, [#10]): `GET /api/v1/usernames/{username}` is
  removed — use the paginated `/api/v1/usernames/search`. `/usernames/available`
  serves only the v1 `{_tag, value}` shape; the flat v0 record is gone. The
  usernames surface returns ordinary 4xx problem bodies instead of the previous
  dialect, and several quirk 500s are now 400s.
- Username registration is authorized by `candidateSignature` rather than the JWT
  subject ([#3]).

[Unreleased]: https://github.com/paritytech/identity-backend-rust/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/paritytech/identity-backend-rust/releases/tag/v0.1.2

[#1]: https://github.com/paritytech/identity-backend-rust/pull/1
[#3]: https://github.com/paritytech/identity-backend-rust/pull/3
[#4]: https://github.com/paritytech/identity-backend-rust/pull/4
[#5]: https://github.com/paritytech/identity-backend-rust/pull/5
[#7]: https://github.com/paritytech/identity-backend-rust/pull/7
[#8]: https://github.com/paritytech/identity-backend-rust/pull/8
[#9]: https://github.com/paritytech/identity-backend-rust/pull/9
[#10]: https://github.com/paritytech/identity-backend-rust/pull/10
[#12]: https://github.com/paritytech/identity-backend-rust/pull/12
[#13]: https://github.com/paritytech/identity-backend-rust/pull/13
[#14]: https://github.com/paritytech/identity-backend-rust/pull/14
[#15]: https://github.com/paritytech/identity-backend-rust/pull/15
[#16]: https://github.com/paritytech/identity-backend-rust/pull/16
[#22]: https://github.com/paritytech/identity-backend-rust/pull/22
[#23]: https://github.com/paritytech/identity-backend-rust/pull/23
[#24]: https://github.com/paritytech/identity-backend-rust/pull/24
[#26]: https://github.com/paritytech/identity-backend-rust/pull/26
[#29]: https://github.com/paritytech/identity-backend-rust/pull/29
[#31]: https://github.com/paritytech/identity-backend-rust/pull/31
[#32]: https://github.com/paritytech/identity-backend-rust/pull/32
[#33]: https://github.com/paritytech/identity-backend-rust/pull/33
[#46]: https://github.com/paritytech/identity-backend-rust/pull/46
