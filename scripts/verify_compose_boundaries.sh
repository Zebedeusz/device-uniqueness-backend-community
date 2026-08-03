#!/usr/bin/env bash
set -euo pipefail

rendered="$(docker compose config)"

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

# DotNS request validation belongs only to identity-api. Compose uses explicit
# environment allowlists, so require all three knobs there and keep them out of
# unrelated processes.
for key in DOTNS_GATEWAY_ENABLED DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS DOTNS_MAX_FUTURE_SKEW_SECS; do
  require_key identity-api "$key"
  for service in identity-chain-writer registration-queue username-indexer dim-tickets-api \
                 dim-tickets-writer invite-tickets-api invite-tickets-pool turn-api gateway; do
    forbid_key "$service" "$key"
  done
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
               invite-tickets-pool turn-api notify-relay gateway \
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

if ! service_block dim-tickets-postgres | grep -Fq "host_ip: 127.0.0.1"; then
  echo "dim-tickets-postgres host port must be loopback-only" >&2
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

# The gateway terminates and routes only — it must never hold any secret.
for key in JWT_ED25519_SECRET CHAIN_WRITER_SIGNER_SURI INVITER_SIGNER_SURI \
           INVITE_INVITER_SIGNER_SURI TURN_SECRET DATABASE_URL JWT_JWKS_JSON \
           JWT_ED25519_PUBLIC_KEY APNS_PRIVATE_KEY APNS_PRIVATE_KEY_FILE \
           FCM_SERVICE_ACCOUNT_JSON; do
  forbid_key gateway "$key"
done

echo "compose configuration boundaries: ok"
