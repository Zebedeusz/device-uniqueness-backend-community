> Stage: active. Check boxes as you go. Mark a phase done when its ✅ demo passes.

# Goals
- Know when the attester runs out of allowance or funds, before users do.
- Know when the chain writer stops making progress.
- Get told in Matrix, not by reading logs.

# Non-goals
- Metrics for everything. Start with the two things that silently break registration.
- Grafana, dashboards, OpenTelemetry, Sentry. Later, if we miss them.
- Instrumenting services that aren't on fire.

# Phase 1 — Read the allowance (half a day) — done

`PeopleLite::AttestationAllowance` is never read at runtime. If it hits zero, registration
stops and nothing says so.

- [x] Add `attestation_allowance()` + reuse `free_balance()` in `identity-service/src/chain/client.rs`
- [x] Poll both every 60s in `identity-chain-writer`, log one INFO line per tick
      (`ATTESTER_RESOURCE_POLL_SECS`, first tick on lease acquisition)
- [x] WARN below a configured floor (`ATTESTER_ALLOWANCE_FLOOR`,
      `ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK`)
- [x] Note: when the writer proxies for `ATTESTER_ACCOUNT`, allowance is on the **authority**, fees on the **signer**
      — the pass reads the primary's allowance and both free balances

✅ **Demo passed** (local stack against Paseo Next v2, 2026-08-04):
`docker compose logs identity-chain-writer | grep attester_resources` →
`allowance=962358 allowance_account=0x86aa… signer_balance_planck=97061295465369
primary_balance_planck=99976052651206`. Restarted with the floors raised above those values
and both WARNs fired (`attestation allowance below floor`, `signer balance below floor`).

# Phase 2 — /metrics (a day) — done

- [x] Add `metrics` + Prometheus exporter, serve `/metrics` on port 9090 (not through Caddy — the edge is the only thing that publishes host ports)
      — shared `http_common::metrics::spawn`, installed by all ten processes including the headless workers
- [x] Export: allowance, balances, finalized head, indexer checkpoint lag, outbox depth, oldest queued row
      (`ibv2_attester_allowance`, `ibv2_account_free_balance_planck{role,chain}`,
      `ibv2_chain_finalized_head_block`, `ibv2_indexer_checkpoint_lag_blocks`,
      `ibv2_outbox_depth{status}`, `ibv2_outbox_oldest_age_seconds{status}`)
- [x] Add Prometheus in its own compose project on a private network
      (`observability/`, external `ibv2-metrics`, UI loopback-only on 9091)

✅ **Demo passed** (local stack against Paseo Next v2, 2026-08-04): all six metrics present —
writer reports `ibv2_attester_allowance 962354`, both balances and all six outbox statuses;
indexer reports `ibv2_chain_finalized_head_block 2241895`,
`ibv2_indexer_checkpoint_block 2241859`, `ibv2_indexer_checkpoint_lag_blocks 36`.
Prometheus targets page: 11/11 `up` (ten services + itself).

Notes for Phase 3: the exporter is best-effort — a bind failure degrades to no metrics rather
than killing the process. The writer's gauge pass runs inside the lease-holding loop, so a
writer waiting on another instance's lease reports nothing; absence of `ibv2_outbox_depth` is
itself the "no writer is working" signal.

# Phase 2b — Readiness gauges (half a day) — done

Prometheus' `up` only proves the metrics port answered. A service whose Postgres died still
scrapes as `up`, so Phase 3 could not have alerted on it.

- [x] `http_common::metrics::spawn_readiness_probe` runs each service's existing `/readyz`
      dependency probe every 30s and publishes `ibv2_readyz_ok` (1/0) plus
      `ibv2_dependency_up{component}`
- [x] The probe is shared with the endpoint (extracted as `health::probe`), so gauge and
      endpoint can never disagree
- [x] Extended to the four workers, which serve no HTTP and so had no readiness signal at all

✅ **Demo passed** (2026-08-04): all ten processes report `ibv2_readyz_ok 1`. Stopping
`dim-tickets-postgres` flipped `dim-tickets-api` to `ibv2_readyz_ok 0` +
`ibv2_dependency_up{component="db"} 0` **while Prometheus still reported `up 1`** — the exact
blind spot this closes. Restarting Postgres returned both to 1 and 11/11 targets up.

Phase 3 rule to add from this: alert on `ibv2_readyz_ok == 0` for 5m, distinct from `up == 0`
(process gone) — and note `dim-tickets-writer` went `up 0` instead, because it crash-loops on a
dead database rather than staying up degraded.

# Phase 3 — Actually get paged (half a day)
- [ ] Alertmanager + a Matrix bot in the backend/ops room
- [ ] Four rules: allowance zero, signer balance low, finalized head stuck 5min, outbox has work but no progress 5min
- [ ] Fifth rule: `ibv2_readyz_ok == 0` for 5min (a dependency died while the process stayed up)

✅ **Demo**: stop `identity-chain-writer` with rows in the outbox. Phone buzzes. Start it. Get the resolved message.

# Later
- Per-service metrics for DIM/invite/notify (fold in the deferrals already noted in those plans)
- Grafana, once there are queries we keep re-running by hand
- Sentry, once we have an unexplained 500

# Open questions
- Who creates the Matrix bot?
- Alert floors are guesses until we have a week of data. Fine — start loud, tune down.
