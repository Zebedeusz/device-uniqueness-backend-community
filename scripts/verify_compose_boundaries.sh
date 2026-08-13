#!/usr/bin/env bash
set -euo pipefail

rendered="$(docker compose config)"
# Cleared so a shell still holding the local-run overrides (see the header of
# gateway/docker-compose.yml) checks the committed defaults, not its own ports.
rendered_gateway="$(EDGE_HTTP_PORT= EDGE_HTTPS_PORT= docker compose -f gateway/docker-compose.yml config)"
rendered_debug="$(docker compose -f docker-compose.yml -f docker-compose.debug.yml config)"
# Same treatment as the gateway: cleared so a shell that republished a UI port
# for itself is checked against the committed loopback defaults.
rendered_observability="$(PROMETHEUS_PORT= GRAFANA_PORT= ALLOY_PORT= \
  docker compose -f observability/docker-compose.yml config)"

service_block() {
  local service="$1"
  awk -v target="$service" '
    $0 == "  " target ":" {
      in_service = 1
      next
    }
    in_service && /^  [a-zA-Z0-9_-]+:$/ {
      exit
    }
    in_service {
      print
    }
  ' <<<"$rendered"
}

environment_keys() {
  service_block "$1" | awk '
    /^    environment:$/ {
      in_environment = 1
      next
    }
    in_environment && /^      [A-Z0-9_]+:/ {
      key = $1
      sub(/:$/, "", key)
      print key
      next
    }
    in_environment && /^    [a-zA-Z0-9_-]+:/ {
      exit
    }
  '
}

require_key() {
  local service="$1"
  local key="$2"
  if ! environment_keys "$service" | grep -Fxq "$key"; then
    echo "$service must receive $key" >&2
    exit 1
  fi
}

# One `host_ip|published->target/protocol` line per published port mapping, read
# from a rendered compose document on stdin. Structural rather than a substring
# count, so a mapping that keeps the host port but changes the target, the
# protocol, or the interface cannot slip through.
port_mappings() {
  awk '
    /^      - mode:/     { host = "*"; next }
    /^        host_ip:/  { host = $2; next }
    /^        target:/   { target = $2; next }
    /^        published:/ { published = $2; gsub(/"/, "", published); next }
    /^        protocol:/ { print host "|" published "->" target "/" $2 }
  ' | sort
}

forbid_key() {
  local service="$1"
  local key="$2"
  if environment_keys "$service" | grep -Fxq "$key"; then
    echo "$service must not receive $key" >&2
    exit 1
  fi
}

for service in identity-api identity-chain-writer username-indexer; do
  forbid_key "$service" INVITER_SIGNER_SURI
done

# The dotNS gateway lane has two halves that must agree, exactly like
# QUEUE_ENABLED. identity-api gates intake on the flag. identity-chain-writer
# submits DotnsGateway::reserve_name behind the same flag. A split value is the
# intake-accepts-but-nothing-submits gap the lane exists to close. The flag is
# therefore REQUIRED on both and forbidden everywhere else.
require_key identity-api DOTNS_GATEWAY_ENABLED
require_key identity-chain-writer DOTNS_GATEWAY_ENABLED
for service in registration-queue username-indexer dim-tickets-api \
               dim-tickets-writer invite-tickets-api invite-tickets-pool turn-api; do
  forbid_key "$service" DOTNS_GATEWAY_ENABLED
done

# Request-validation bounds are intake-only. They shape 400s, not extrinsics.
for key in DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS DOTNS_MAX_FUTURE_SKEW_SECS; do
  require_key identity-api "$key"
  for service in identity-chain-writer registration-queue username-indexer dim-tickets-api \
                 dim-tickets-writer invite-tickets-api invite-tickets-pool turn-api; do
    forbid_key "$service" "$key"
  done
done

# The Asset Hub connection and the attester identity belong to the writer. It is
# the only process that submits there. ATTESTER_ACCOUNT names the authority on
# both chains and the writer derives proxying from it, so the two must be one
# value. identity-api needs it too, since it serves GET /api/v1/attester.
# Nothing else does, and no other service opens an Asset Hub connection.
require_key identity-chain-writer ASSET_HUB_RPC_URL
require_key identity-chain-writer ATTESTER_ACCOUNT
require_key identity-api ATTESTER_ACCOUNT
for service in registration-queue username-indexer dim-tickets-api \
               dim-tickets-writer invite-tickets-api invite-tickets-pool turn-api \
               notify-relay; do
  forbid_key "$service" ASSET_HUB_RPC_URL
done

# registration-queue only reads chain balances and flips queue rows in the
# identity DB — never signing secrets or JWT material. Its lease is the queue
# liveness signal, so it needs its cadence knobs.
for key in JWT_ED25519_SECRET JWT_ED25519_PUBLIC_KEY JWT_JWKS_JSON \
           CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI INVITE_INVITER_SIGNER_SURI \
           TURN_SECRET; do
  forbid_key registration-queue "$key"
done
require_key registration-queue QUEUE_ADVANCE_INTERVAL_SECS
require_key registration-queue QUEUE_LEASE_TTL_SECS
# The wires the queue's throttle story hinges on: intake gating on the api,
# and the SAME flag on the writer (on = hold the throttle, warn about
# stranded rows; off = janitor drain after the grace window). Split values
# would strand-or-unthrottle depending on which side is on.
require_key identity-api QUEUE_ENABLED
require_key identity-chain-writer QUEUE_ENABLED
require_key identity-chain-writer QUEUE_FALLBACK_AFTER_SECS

# username-indexer serves public reads and holds VERIFY-ONLY JWT material: the
# public key admits authenticated callers past the proof-of-compute gate, but the
# service can never mint a token. The signing secret and every other service's
# secret stay out.
for key in JWT_ED25519_SECRET CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI \
           INVITE_INVITER_SIGNER_SURI TURN_SECRET; do
  forbid_key username-indexer "$key"
done
require_key username-indexer SEARCH_RATE_LIMIT
require_key username-indexer SEARCH_RATE_LIMIT_WINDOW_SECS
# The gate's flag and its two key inputs: the bypass needs verify-only JWT
# material, and the puzzle secret must live here and nowhere else.
require_key username-indexer POC_ENABLED
require_key username-indexer JWT_ED25519_PUBLIC_KEY
require_key username-indexer POC_HMAC_SECRET
# Every other service in the compose file, application and database alike.
for service in identity-api identity-chain-writer registration-queue \
               dim-tickets-api dim-tickets-writer invite-tickets-api \
               invite-tickets-pool turn-api notify-relay \
               postgres username-indexer-postgres dim-tickets-postgres \
               invite-tickets-postgres; do
  forbid_key "$service" POC_HMAC_SECRET
done

for service in dim-tickets-api dim-tickets-writer; do
  forbid_key "$service" JWT_ED25519_SECRET
  forbid_key "$service" CHAIN_WRITER_SIGNER_SURI
done
forbid_key dim-tickets-api INVITER_SIGNER_SURI

require_key dim-tickets-writer PEOPLE_NETWORK
require_key dim-tickets-writer INVITER_ADDRESS
require_key dim-tickets-writer INVITER_SIGNER_SURI

for key in JWT_JWKS_JSON JWT_ED25519_PUBLIC_KEY QUOTA_TIMEOUT_SECS RATE_LIMIT RATE_LIMIT_WINDOW_SECS; do
  require_key dim-tickets-api "$key"
done

for key in WRITER_LEASE_TTL_SECS WRITER_POLL_SECS WRITER_BATCH_SIZE WRITER_MAX_RETRIES WRITER_RETRY_BASE_MS WRITER_RETRY_MAX_MS WRITER_RETRY_MAX_EXPONENT WRITER_FINALIZE_SECS; do
  require_key dim-tickets-writer "$key"
done

# Only the edge publishes. The base file must expose nothing on the host, so a
# second environment cannot collide and no service is reachable from outside.
if grep -Fq "published:" <<<"$rendered"; then
  echo "no service in docker-compose.yml may publish a host port (the edge does that)" >&2
  grep -B2 -F "published:" <<<"$rendered" >&2
  exit 1
fi

# The debug overlay exists to reopen those ports locally; it must never widen
# them beyond loopback, and it must stay useful — an overlay that quietly lost
# most of its mappings would still satisfy a "nothing public" check.
debug_mappings="$(port_mappings <<<"$rendered_debug")"
debug_count="$(grep -c . <<<"$debug_mappings" || true)"
debug_public="$(grep -vc '^127\.0\.0\.1|' <<<"$debug_mappings" || true)"
debug_unique="$(cut -d'|' -f2 <<<"$debug_mappings" | cut -d'-' -f1 | sort -u | grep -c . || true)"
if [ "$debug_count" -ne 10 ]; then
  echo "docker-compose.debug.yml must republish all 10 service/Postgres ports ($debug_count found)" >&2
  echo "$debug_mappings" >&2
  exit 1
fi
if [ "$debug_public" -ne 0 ]; then
  echo "docker-compose.debug.yml may only publish on 127.0.0.1:" >&2
  grep -v '^127\.0\.0\.1|' <<<"$debug_mappings" >&2
  exit 1
fi
if [ "$debug_unique" -ne "$debug_count" ]; then
  echo "docker-compose.debug.yml publishes the same host port twice:" >&2
  echo "$debug_mappings" >&2
  exit 1
fi

# notify-relay is verify-only and holds its own push secrets, but never another
# service's signing/DB secrets.
for key in JWT_ED25519_SECRET CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI \
           INVITE_INVITER_SIGNER_SURI TURN_SECRET DATABASE_URL; do
  forbid_key notify-relay "$key"
done
require_key notify-relay JWT_ED25519_PUBLIC_KEY
require_key notify-relay NOTIFY_RATE_LIMIT
require_key notify-relay NOTIFY_RATE_LIMIT_WINDOW_SECS

# The gateway terminates and routes only — it must never hold any secret. It
# lives in its own compose project: the helpers read `rendered`, so the swap is
# scoped to a subshell rather than left in place for whatever is added below.
(
  rendered="$rendered_gateway"
  for key in JWT_ED25519_SECRET CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI \
             INVITE_INVITER_SIGNER_SURI TURN_SECRET DATABASE_URL JWT_JWKS_JSON \
             JWT_ED25519_PUBLIC_KEY APNS_PRIVATE_KEY APNS_PRIVATE_KEY_FILE \
             FCM_SERVICE_ACCOUNT_JSON POC_HMAC_SECRET DOTNS_GATEWAY_ENABLED \
             ASSET_HUB_RPC_URL DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS \
             DOTNS_MAX_FUTURE_SKEW_SECS; do
    forbid_key caddy "$key"
  done
)

# The edge is the one thing that may publish, and only the two public ports —
# target and protocol included, so neither losing 443/udp nor pointing 443 at
# the wrong container port can pass.
edge_published="$(port_mappings <<<"$rendered_gateway")"
expected_published="$(printf '*|443->443/tcp\n*|443->443/udp\n*|80->80/tcp\n' | sort)"
if [ "$edge_published" != "$expected_published" ]; then
  echo "the edge must publish exactly 80->80/tcp, 443->443/tcp and 443->443/udp, got:" >&2
  echo "$edge_published" >&2
  exit 1
fi

# Monitoring is for operators behind an SSH tunnel, never for the internet: the
# three UIs (Grafana, Prometheus, Alloy) may publish only on loopback, and Loki
# not at all — Alloy writes to it and Grafana reads it over the metrics network.
observability_published="$(port_mappings <<<"$rendered_observability")"
expected_observability="$(printf '127.0.0.1|12345->12345/tcp\n127.0.0.1|3000->3000/tcp\n127.0.0.1|9091->9090/tcp\n' | sort)"
if [ "$observability_published" != "$expected_observability" ]; then
  echo "the observability project must publish only Grafana/Prometheus/Alloy on 127.0.0.1, got:" >&2
  echo "$observability_published" >&2
  exit 1
fi

# It reads logs and metrics; it must never be handed anything it could sign or
# decrypt with. The Grafana admin password is its own and stays out of this set.
(
  rendered="$rendered_observability"
  for service in prometheus loki alloy grafana; do
    for key in JWT_ED25519_SECRET CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI \
               INVITE_INVITER_SIGNER_SURI TURN_SECRET POC_HMAC_SECRET DATABASE_URL \
               APNS_PRIVATE_KEY FCM_SERVICE_ACCOUNT_JSON; do
      forbid_key "$service" "$key"
    done
  done
)

echo "compose configuration boundaries: ok"
