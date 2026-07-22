# Identity Backend V2 roadmap

This file is the small index of work required to retire the legacy backend. It is not an implementation plan. Each substantial slice gets its own ship-first plan when work starts, so one owner can deliver it without changing another service's plan.

## Planning rules

- One independently deployable service or bounded capability per plan.
- Each service owns its persistence if it has any, plus its secrets, health, tests, and deployment configuration. Stateless services should not add a database without a proven requirement.
- No service reads another service's tables.
- JWT/JWKS and the frozen mobile HTTP contracts are the only cross-service runtime contracts.
- Correctness and security belong in each MVP. Measured optimization and cross-service hardening happen before cutover.

## Done

- Identity Service MVP: `docs/plans/done/identity-service-mvp.md`.
- Username read/search indexer: `docs/plans/done/username-indexer.md`.

## Active independent slices

- Notification relay: `docs/plans/active/notify-relay.md`.
- DIM tickets (game/personhood) for `/api/v1/dim-ticket`: `docs/plans/active/dim-tickets.md`.
  Note (2026-07-22): this route has no known client consumer — the shipping apps obtain
  Game/ProofOfInk credentials via `/api/v1/invitation-ticket/claim` (see the plan's correction
  note), so the invite-claim slice below is the app-compatibility priority.

## Required later slices

Create a focused ship-first plan when each item is ready to start:

1. TURN compatibility for the shipping `/api/v1/turn/issue` route.
2. Invite faucet compatibility for `/api/v1/invitation-ticket/claim` — the route the shipping
   iOS/Android apps actually call (`POST` with `{who, dim}` for both Game and ProofOfInk;
   synchronous signature response). App-compatibility priority among these slices.
3. Single-URL gateway and per-service deployment integration.
4. iOS App Attest and DeviceCheck verification.
5. Android Play Integrity and TEE verification.
6. Registration eligibility, queue, payment, and voucher behavior.
7. Production hardening: real batching if measurements require it, load tests, metrics, alerts, and parity benchmarks.
8. iOS and Android release-device validation.
9. Data migration, rollback, route-by-route cutover, and legacy shutdown.

The legacy backend is retired only after every required route has an owner, the gateway is validated, parity gates pass, rollback is proven, and release-device smokes succeed.