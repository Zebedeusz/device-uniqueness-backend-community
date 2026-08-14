#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <db|chain|providers> <cargo command...>" >&2
  exit 2
}

mode="${1:-}"
if [ "$#" -lt 2 ]; then
  usage
fi
shift
cargo_cmd=("$@")

if [ "$mode" = "providers" ]; then
  ran_provider=false
  if { [ -n "${APNS_PRIVATE_KEY:-}" ] || \
       { [ -n "${APNS_PRIVATE_KEY_FILE:-}" ] && [ "${APNS_PRIVATE_KEY_FILE}" != "/dev/null" ]; }; }; then
    "${cargo_cmd[@]}" -p notifications --test apns_live -- --ignored --nocapture
    ran_provider=true
  fi
  if [ -n "${FCM_SERVICE_ACCOUNT_JSON:-}" ]; then
    "${cargo_cmd[@]}" -p notifications --test fcm_live -- --ignored --nocapture
    ran_provider=true
  fi
  if [ "$ran_provider" = false ]; then
    echo "configure APNS_PRIVATE_KEY/APNS_PRIVATE_KEY_FILE or FCM_SERVICE_ACCOUNT_JSON" >&2
    exit 2
  fi
  exit 0
fi

if [ "$mode" != "db" ] && [ "$mode" != "chain" ]; then
  usage
fi

# A dedicated, per-invocation project keeps test containers, networks, and
# named volumes separate from the normal identity-backend-v2 dev stack. The
# prefix check makes the EXIT cleanup safe even if an override is supplied.
test_project="${IBV2_TEST_COMPOSE_PROJECT:-identity-backend-v2-tests-$$}"
case "$test_project" in
  identity-backend-v2-tests-*) ;;
  *)
    echo "IBV2_TEST_COMPOSE_PROJECT must start with identity-backend-v2-tests-" >&2
    exit 2
    ;;
esac

identity_port="${IDENTITY_TEST_POSTGRES_PORT:-56432}"
indexer_port="${INDEXER_TEST_POSTGRES_PORT:-56433}"
dim_port="${DIM_TICKETS_TEST_POSTGRES_PORT:-56434}"
invite_port="${INVITE_TICKETS_TEST_POSTGRES_PORT:-56435}"
export IDENTITY_POSTGRES_PORT="$identity_port"
export INDEXER_POSTGRES_PORT="$indexer_port"
export DIM_TICKETS_POSTGRES_PORT="$dim_port"
export INVITE_POSTGRES_PORT="$invite_port"

compose=(
  docker compose
  -p "$test_project"
  -f docker-compose.yml
  -f docker-compose.debug.yml
)
databases=(
  postgres
  username-indexer-postgres
  invite-tickets-postgres
  dim-tickets-postgres
)

cleanup() {
  "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

"${compose[@]}" up -d --wait --wait-timeout 90 "${databases[@]}"

identity_url="postgres://identity:identity@localhost:${identity_port}/identity"
indexer_url="postgres://username_indexer:username_indexer@localhost:${indexer_port}/username_indexer"
invite_url="postgres://invite_tickets:invite_tickets@localhost:${invite_port}/invite_tickets"
dim_url="postgres://dim_tickets:dim_tickets@localhost:${dim_port}/dim_tickets"

if [ "$mode" = "chain" ]; then
  # These production-router suites also require PEOPLE_RPC_URL, or use their
  # public Paseo default. They stay opt-in because external chain availability
  # is not a deterministic merge/release prerequisite.
  for suite in voucher_http_live payment_http_live payment_watch_live; do
    IDENTITY_TEST_DATABASE_URL="$identity_url" \
      "${cargo_cmd[@]}" -p identity-service --test "$suite" -- --ignored
  done
  exit 0
fi

# Deterministic database-only gate: 14 suites / 38 ignored tests. Keep this
# list here so local tests, CI, and coverage all execute the same catalog.
for suite in allocation_live auth_live outbox_live dotns_live queue_live voucher_live payment_live; do
  IDENTITY_TEST_DATABASE_URL="$identity_url" \
    "${cargo_cmd[@]}" -p identity-service --test "$suite" -- --ignored
done

# `ingest_live` and `chain_identity_live` both drive the singleton `sync_state`
# row, and `chain_identity_live` clears `assigned_usernames` wholesale — this
# loop runs one suite at a time, which is what keeps them from interleaving.
for suite in pagination_live poc_gate_live ingest_live chain_identity_live; do
  DATABASE_URL="$indexer_url" \
    "${cargo_cmd[@]}" -p username-indexer --test "$suite" -- --ignored
done

# This suite truncates its table per test, so Cargo test threads must not race.
INVITE_TICKETS_TEST_DATABASE_URL="$invite_url" \
  "${cargo_cmd[@]}" -p invite-tickets --test claim_live_pg -- --ignored --test-threads=1
INVITE_TICKETS_TEST_DATABASE_URL="$invite_url" \
  "${cargo_cmd[@]}" -p invite-tickets --test pool_live_pg -- --ignored

DIM_TICKETS_TEST_DATABASE_URL="$dim_url" \
  "${cargo_cmd[@]}" -p dim-tickets --test tickets_live_pg -- --ignored
