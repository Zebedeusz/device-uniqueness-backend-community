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
