# Changelog

Notable changes to the published images — `docker.io/paritytech/ibv2-<service>` publicly,
`ghcr.io/paritytech/identity-backend-rust/<service>` for our own deploys. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0, a breaking change bumps the **minor**. External consumers should pin an
exact `vX.Y.Z`; `stable` tracks the newest release and `latest` tracks `main`
(unreleased work — see [README](README.md#releases)).

Entries at or before 0.1.2 predate this file and are reconstructed from history:
they summarise each release rather than list every change.

## [Unreleased]

### Changed

- **BREAKING (consumers): one image per service.** The single multi-binary image
  `ghcr.io/paritytech/identity-backend-rust:<tag>` is no longer published. Each of the ten
  binaries now ships as its own image, `ghcr.io/paritytech/identity-backend-rust/<service>:<tag>`,
  holding exactly that binary — smaller images, per-service vulnerability scanning, and an
  independent rollback per service. **All ten always carry the same tag**; a per-service pin is
  not supported. Anyone pinning the old path must move to the per-service repos and drop any
  `command:` override, since each image's `CMD` is now its own binary. The compose bundle attached
  to each release resolves the new paths from `IMAGE_REGISTRY`/`IMAGE_TAG`.

### Added

- Released images are mirrored to **public Docker Hub** — `docker pull
  paritytech/ibv2-identity-api:stable` needs no account. `vX.Y.Z` and `stable` are on both
  registries; `latest` stays ghcr-only, since it tracks unreleased `main`. Docker Hub
  namespaces are one level, so the mirror is `paritytech/ibv2-<service>`, not a nested path
  ([#50]).
- Every release attaches per-platform binaries (`ibv2-<version>-<target>.tar.gz` for
  x86_64 / aarch64 Linux and Apple Silicon), exported from the same builder layer the images
  are built from, so the tarballs and the images hold the same bytes ([#50]).
- The released `ibv2-compose-*.tar.gz` bundle now starts without a source tree: its
  `docker-compose.yml` drops every `build:` stanza and pins all ten images to that release,
  so `docker compose up -d` needs neither `--no-build` nor a manually set tag ([#50]).
- Kubernetes deployment: a Helm chart under `helm/` rendering the ten workloads, per-workload
  secrets, and the route table as a Traefik `IngressRoute`. See
  [`docs/deploy.md`](docs/deploy.md#kubernetes-helm).

### Added

- Prometheus metrics for the invite-tickets pool: unclaimed tickets per DIM
  (`ibv2_invite_pool_depth`), the inviter's on-chain quota
  (`ibv2_inviter_available_invites`), its fee balance on the shared
  `ibv2_account_free_balance_planck` family, and its registration outcomes on
  `ibv2_chain_submit_total{lane="invite"}`.
- Prometheus counters and gauges for the chain writer's submissions
  (`ibv2_chain_submit_total` by lane and outcome, zero-initialised at startup),
  the connected runtimes (`ibv2_chain_spec_version`,
  `ibv2_chain_transaction_version`), and the deployment's own identity
  (`ibv2_writer_info`: signer, attester, dotNS lane).
- Metrics and logs are scoped per environment: Prometheus scrapes a job per
  environment labelled `env_id`, log streams carry the same label, and the
  overview dashboard selects one environment across both datasources.
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

- The outbox "oldest age" gauges measure time in the current status rather than
  the age of the row, so a retried or newly promoted claim no longer reports as
  hours old and the terminal statuses no longer grow without bound.
- `ibv2_dependency_up` no longer reports components as up when the readiness
  probe short-circuited before checking them; they are published as `NaN`.
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
[#50]: https://github.com/paritytech/identity-backend-rust/pull/50
