# Changelog

Notable changes to the published image — `docker.io/paritytech/device-uniqueness-backend`
publicly, `ghcr.io/paritytech/device-uniqueness-backend` for our own deploys. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0, a breaking change bumps the **minor**. External consumers should pin an
exact `vX.Y.Z`; `stable` tracks the newest release and `latest` tracks `main`
(unreleased work — see [README](README.md#releases)).

Entries at or before 0.1.2 predate this file and are reconstructed from history:
they summarise each release rather than list every change.

## [Unreleased]

### Removed

- **BREAKING (API): every V1 wire dialect is gone. Error bodies across the whole
  surface are now one JSON envelope.** Nothing reproduces the legacy TypeScript
  backend byte-for-byte any more, so a client that parses V1 error bodies must be
  updated in step.
  - `{"error": "<message>"}` at every status, always `application/json`, with an
    optional `"fields": [{"field", "message"}]` array on per-field validation
    failures. Replaces the RFC 9457 problem-details bodies
    (`application/problem+json`, `type`/`title`/`detail`/`status`/`errors[]` with
    `#/`-prefixed JSON pointers), the Hono plain-text bodies (`404 Not Found`,
    `Malformed JSON in request body`), and the opaque
    `{"error":"Oops! Something went wrong."}` 500.
  - Field messages are now plain English (`must not be empty`,
    `expected string, received nothing`, `must contain at most 100 items`) where
    they were verbatim zod v4 strings, and field identifiers are bare names
    (`who`, `dotns.signedAt`, `usernames[0]`) where they were JSON pointers.
  - `turn`'s proof routes move onto the same envelope; they were the last
    `application/problem+json` responses in the workspace.
  - **Behavioral quirks inherited from V1 are gone.** A request with no JSON
    content type is validated like any other (it no longer skips validation into
    a `201` on `turn/issue`, nor crashes into a `500` on
    `invitation-ticket/claim`); `limit` on username search is parsed as an
    integer instead of through JS `Number()` semantics; search timestamps
    serialize as RFC 3339 (`…T10:00:00Z`) instead of `Date#toISOString`
    milliseconds.
  - `POST /api/v1/usernames/available` drops its `?version` query parameter (it
    existed only to reject the removed `v0` shape); the response is unchanged.
  - `GET /api/v1/usernames/search` drops the inert `includeOnchainData`
    parameter and the always-null `onchainData` response field.
  - The four `legacy_wire.rs` golden-replay suites and their captured
    `legacy-wire.json` fixtures (plus the TypeScript capture harnesses) are
    deleted, along with `http-common`'s `problem` and `zod` modules.
- **BREAKING (deployment): pre-namespacing environment variable names are no
  longer read.** `IDENTITY_DATABASE_URL` and the bare `DATABASE_URL`,
  `RATE_LIMIT`, and `RATE_LIMIT_WINDOW_SECS` were accepted with a deprecation
  `WARN`; an environment still setting only those now fails fast at boot naming
  the namespaced key (`DEVICE_ATTESTATION_DATABASE_URL`,
  `INDEXER_DATABASE_URL`, `INVITE_TICKETS_DATABASE_URL`,
  `INVITE_TICKETS_RATE_LIMIT`, `TURN_RATE_LIMIT`, …).
- The stale `LEGACY_BACKEND_ORIGIN` entry in `.env.example`; the Caddy lane it
  configured was removed on 2026-08-03.

### Changed

- **BREAKING (deployment): the identity service is renamed device-attestation,
  including its Postgres role and database.** `POSTGRES_USER`/`POSTGRES_DB` and
  every service's `DEVICE_ATTESTATION_DATABASE_URL` now default to
  `device_attestation` where they said `identity`.
  - **An existing volume does not migrate itself.** The postgres image reads
    `POSTGRES_*` only when it initialises an empty data directory, so a volume
    created under the old names keeps them and the new defaults would fail
    authentication.
  - Three new compose vars are the supported fix, and the only knob for this:
    `DEVICE_ATTESTATION_DB_USER`, `DEVICE_ATTESTATION_DB_PASSWORD` and
    `DEVICE_ATTESTATION_DB_NAME`. They are what the `postgres` service
    initialises with *and* what each service's URL is built from, so setting all
    three to `identity` keeps an existing database in place with no downtime and
    no SQL. `DEVICE_ATTESTATION_DATABASE_URL` in `.env` does **not** reconfigure
    the containers — it is the host-side string for `cargo run`. Both migration
    paths are in `docs/runbook.md`, "One-time: the identity -> device-attestation
    rename".
  - The config key `IDENTITY_DATABASE_URL` → `DEVICE_ATTESTATION_DATABASE_URL`
    (and, before that, a bare `DATABASE_URL`). The old names were dropped in the
    same release cycle — see Removed above.
- **BREAKING (deployment): every `ibv2` identifier is renamed `dub`, after the
  repository's move to `device-uniqueness-backend`.** `ibv2` encoded "identity
  backend v2" — a name nothing in the repository explains and no new reader can
  decode. Entries at and before 0.2.0 keep the old spelling: they describe
  artifacts already published under it.
  - The binary is `dub` (crate `crates/dub`), so `command:` becomes
    `["--role", "identity-api"]` on `dub`, healthchecks are `dub --healthcheck`,
    and `dub --list-roles` prints the ten. Release assets become
    `dub-<version>-<target>.tar.gz` and `dub-compose-<version>.tar.gz`; the
    `ibv2-*` assets on 0.1.2 and 0.2.0 keep their names.
  - **Every Prometheus metric is renamed** `ibv2_*` → `dub_*`
    (`dub_readyz_ok`, `dub_dependency_up`, `dub_chain_submit_total`, the
    `dub_dotns_*` family, …). Any dashboard or alert rule outside this
    repository must be retargeted; the committed Grafana dashboard moves to
    `observability/grafana/dashboards/dub-overview.json` (uid `dub-overview`)
    and is already updated.
  - The Helm chart is `dub`, and each environment's compose project is now
    named for that environment: `dub-paseo-next-v2` and `dub-previewnet`. So
    **named volumes move** (`identity-backend-v2_pgdata` →
    `dub-paseo-next-v2_pgdata`) and an existing stack would come up on empty
    databases. The shared edge network is `dub-edge`, and the metrics/log
    networks are `dub-metrics` / `dub-loki`.
  - Helm: chart `dub`, `app.kubernetes.io/part-of: dub`, and the
    env-id annotation `dub.parity.io/env-id`. The selector (`name` + `instance`)
    is unchanged and `part-of` is metadata, so an in-place `helm upgrade` is
    fine. Renaming the **release** is not: `instance` is a selector label and is
    immutable on a live Deployment, so `helm template ibv2` → `dub` (see
    `docs/deploy.md`) means uninstall and reinstall.
  - TURN domain-separation labels become `dub/turn-credential/v1` and
    `dub/turn-proof-credential-id/v1`, changing derived proof credential ids.
    Safe today: `TURN_PROOF_ENABLED` is `false` in every environment, so none
    were ever issued.
  - `IBV2_TEST_COMPOSE_PROJECT` → `DUB_TEST_COMPOSE_PROJECT`. The
    `IBV2_SSH_KEY` / `IBV2_HOST` **CI secret names are unchanged** — renaming
    them needs the repository secrets recreated first.
  - The proof-of-compute HKDF label
    (`identity-backend/proof-of-compute/hmac-v1`) is deliberately untouched: it
    is wire-compatible with the legacy TypeScript service.

## [0.2.0] - 2026-08-17

Breaking on three fronts: the image's ten binaries become one `ibv2` binary
picked with `--role`, per-service config keys are namespaced, and the
proxy-signing and attester-account variables are removed or renamed — read
**Changed** before upgrading a deployment.

### Added

- **`ibv2 --role all-in-one`**: the entire public API in one process on one
  port, with the route table compiled in — no edge, and nothing to reconstruct
  from a Caddyfile. Verified byte-identical to the ten-service stack behind the
  real edge across a 24-row sweep of the route table. It serves `/docs` itself
  and an aggregate `/readyz` naming every component ([#58]).

  **Not a deployable workload**: it holds the union of every secret in the
  system in one address space, so it is absent from `--list-roles` and the
  deployment gate rejects it. It is for local development and for consumers
  self-hosting the API.

- **A second, opt-in deployment topology: five workloads instead of ten.** One
  `all-in-one` process serves all six HTTP surfaces; the four single-instance
  workers stay separate. Example values in `helm/values-small.yaml` ([#58]).

  It trades the standard topology's secret compartmentalisation: one process
  holds `JWT_ED25519_SECRET` (mint a token for any subject), `POC_HMAC_SECRET`,
  `TURN_SECRET` and all four database URLs, in the same address space that
  serves the unauthenticated public search endpoint. Read
  `docs/architecture.md` "Deployment topologies" before using it. The two
  topologies are mutually exclusive, selecting the small one requires
  `global.allInOne.acknowledgedSecretUnion: true`, and the chart refuses to
  render a mix, a missing acknowledgement, or a leftover per-service secret.
  **Nothing changes for existing deployments**: the standard topology is the
  default and is unaffected.

  In the small topology `/readyz` is *degraded-but-ready* — it reports every
  component and answers `200 degraded` while some are down, rather than removing
  the whole API from rotation because one surface's database died. Per-service
  readiness in the standard topology is unchanged.

- **The route table is now generated from one source.** The `(routes)` snippet in
  `gateway/Caddyfile` and the `routes:` list in `helm/values.yaml` are rendered
  by `just routes` from `crates/ibv2/src/routes/table.rs` — the same table
  `--role all-in-one` serves — and a staleness test fails the build if either
  drifts. Both regenerate byte-identically to the previous hand-written files,
  so there is no behaviour change; what changed is that there is one copy of the
  ownership map instead of three ([#58]).
- **`POST /api/v1/turn/issue-with-proof`**: TURN credentials for a proven Lite or
  Full person holding no identity-service JWT — a game or a standalone hosted
  product redeems a Bandersnatch ring-VRF membership proof against a People Chain
  root instead, and gets the same 201 body as `/turn/issue` ([#51]).

  **Off by default** (`TURN_PROOF_ENABLED=false`): the route is not mounted, and
  `/turn/issue` and its JWT contract are untouched. Enabling it requires
  `TURN_PROOF_RPC_URL`, `TURN_PROOF_GENESIS` and `TURN_PROOF_PRODUCTS`, each
  failing closed with no default.

  Redemption is one request with no challenge round trip: the client sends a
  timestamp, the server derives the proved message itself, and accepts it only
  inside a bounded clock skew — nothing is minted or stored. Each request names
  the `productId` it proves for and is verified under the context the server
  derives for that product, so a host does not prompt the user; an unlisted
  product is refused before any verification work. Both canonical collections
  (`people-lite` and `people`) are accepted, each root cache refreshed
  independently in the background — a dead RPC blocks neither boot nor
  `/turn/issue`, and a stale root fails closed. The contextual alias never
  leaves the process: it is the in-memory per-person, per-product throttle key
  and the input to a `TURN_SECRET`-keyed HMAC yielding one opaque credential id.

- The released image is mirrored to **public Docker Hub** — `docker pull
  paritytech/device-uniqueness-backend:stable` needs no account. `vX.Y.Z` and `stable` are on
  both registries; `latest` stays ghcr-only, since it tracks unreleased `main` ([#50],
  [#58]).
- Every release attaches per-platform binaries (`ibv2-<version>-<target>.tar.gz` for
  x86_64 / aarch64 Linux and Apple Silicon), exported from the same builder layer the image
  is built from, so the tarball and the image hold the same bytes ([#50]).
- The released `ibv2-compose-*.tar.gz` bundle now starts without a source tree: its
  `docker-compose.yml` drops every `build:` stanza and pins the image to that release,
  so `docker compose up -d` needs neither `--no-build` nor a manually set tag ([#50],
  [#58]).
- Kubernetes deployment: a Helm chart under `helm/` rendering the ten workloads, per-workload
  secrets, and the route table as a Traefik `IngressRoute`. See
  [`docs/deploy.md`](docs/deploy.md#kubernetes-helm).
- Merging a PR titled `chore(release): vX.Y.Z` tags the merge commit and cuts the
  release itself, after checking that the title, `Cargo.toml` and `CHANGELOG.md`
  agree and the tag is new. Manual `git push --tags` still works unchanged
  ([#53]).
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

- **BREAKING (deployment): the image's ten binaries become one `ibv2` binary,
  and a container picks its service with `--role`.** 0.1.2 shipped one image
  holding ten separate binaries, defaulting to `CMD ["identity-api"]` with a
  `command:` override naming the binary. There is still exactly one image, but
  it holds one binary: every service and worker is a *role* of it —
  `docker run <image> --role identity-api`; `ibv2 --list-roles` prints the ten
  ([#58]).
  - A self-hosted compose file changes `command: ["identity-api"]` to
    `command: ["--role", "identity-api"]`; the released compose bundle already
    carries it.
  - The image has an `ENTRYPOINT` and **no `CMD`**: a container started with no
    arguments exits non-zero listing the roles, rather than silently becoming
    `identity-api` as 0.1.2 did.
  - The image reference moves from `${IDENTITY_IMAGE}` to
    `${IMAGE_REPO}:${IMAGE_TAG}`, where `IMAGE_REPO` is a full repository path
    with no `/<service>` appended. The chart's equivalent is
    `global.imageRepo`.
  - `curl` is gone from the image; container healthchecks use
    `ibv2 --healthcheck`, which probes the container's own `/readyz`.
    Kubernetes `httpGet` probes are unaffected.
  - `scripts/verify_role_split.sh` asserts every compose service and chart
    workload names a role the binary accepts, exactly once.

  No API, wire-format or database change: the public surface is byte-identical,
  and production still runs ten processes with ten per-workload secret sets.

- **BREAKING (configuration): per-service config keys are namespaced.** A bare
  `DATABASE_URL` was read by eight call sites naming **four different Postgres
  instances**, and a bare `RATE_LIMIT` by three services, so tuning or
  misrouting one silently hit the others ([#58]).
  - `DATABASE_URL` → `IDENTITY_DATABASE_URL`, `INDEXER_DATABASE_URL`,
    `DIM_TICKETS_DATABASE_URL`, `INVITE_TICKETS_DATABASE_URL`.
  - `RATE_LIMIT` / `RATE_LIMIT_WINDOW_SECS` → `DIM_TICKETS_*`,
    `INVITE_TICKETS_*`, `TURN_*` (identity and notifications already had
    `AUTH_*` / `NOTIFY_*`).
  - `BIND_ADDR` and `PEOPLE_RPC_URL` are unchanged and stay shared.
  - **The old names still work for this release**, with a `WARN` naming the
    replacement. They will be removed in a later one.

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

- `notify-relay` serves production and sandbox APNs device tokens from one
  instance: `APNS_ENVIRONMENT` now picks the host tried *first*, and a token
  rejected as `400 BadDeviceToken` is retried once against the sibling host.
  `DeviceTokenNotForTopic` and the 410 reasons stay terminal ([#55]).
- The compose stack passes the attestation variables through to `identity-api`,
  which previously started with attestation unconfigured however the host's env
  file was written ([#54]).
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

[Unreleased]: https://github.com/paritytech/device-uniqueness-backend/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/paritytech/device-uniqueness-backend/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/paritytech/device-uniqueness-backend/releases/tag/v0.1.2

[#1]: https://github.com/paritytech/device-uniqueness-backend/pull/1
[#3]: https://github.com/paritytech/device-uniqueness-backend/pull/3
[#4]: https://github.com/paritytech/device-uniqueness-backend/pull/4
[#5]: https://github.com/paritytech/device-uniqueness-backend/pull/5
[#7]: https://github.com/paritytech/device-uniqueness-backend/pull/7
[#8]: https://github.com/paritytech/device-uniqueness-backend/pull/8
[#9]: https://github.com/paritytech/device-uniqueness-backend/pull/9
[#10]: https://github.com/paritytech/device-uniqueness-backend/pull/10
[#12]: https://github.com/paritytech/device-uniqueness-backend/pull/12
[#13]: https://github.com/paritytech/device-uniqueness-backend/pull/13
[#14]: https://github.com/paritytech/device-uniqueness-backend/pull/14
[#15]: https://github.com/paritytech/device-uniqueness-backend/pull/15
[#16]: https://github.com/paritytech/device-uniqueness-backend/pull/16
[#22]: https://github.com/paritytech/device-uniqueness-backend/pull/22
[#23]: https://github.com/paritytech/device-uniqueness-backend/pull/23
[#24]: https://github.com/paritytech/device-uniqueness-backend/pull/24
[#26]: https://github.com/paritytech/device-uniqueness-backend/pull/26
[#29]: https://github.com/paritytech/device-uniqueness-backend/pull/29
[#31]: https://github.com/paritytech/device-uniqueness-backend/pull/31
[#32]: https://github.com/paritytech/device-uniqueness-backend/pull/32
[#33]: https://github.com/paritytech/device-uniqueness-backend/pull/33
[#46]: https://github.com/paritytech/device-uniqueness-backend/pull/46
[#50]: https://github.com/paritytech/device-uniqueness-backend/pull/50
[#51]: https://github.com/paritytech/device-uniqueness-backend/pull/51
[#53]: https://github.com/paritytech/device-uniqueness-backend/pull/53
[#54]: https://github.com/paritytech/device-uniqueness-backend/pull/54
[#55]: https://github.com/paritytech/device-uniqueness-backend/pull/55
[#58]: https://github.com/paritytech/device-uniqueness-backend/pull/58
