#!/usr/bin/env bash
#
# Reset an environment's chain-derived database state after the chain it points
# at has been wiped (PreviewNet is re-spawned routinely) or repointed.
#
# Scope, and why it is not "drop everything":
#
#   deleted  username_reservations  the registration outbox. ASSIGNED rows name
#                                   usernames no chain will confirm, and every
#                                   row — whatever its status — permanently
#                                   occupies a discriminator, because
#                                   `allocated_discriminators` unions the whole
#                                   table into availability with no status
#                                   filter. Left in place, a base with 100
#                                   historical rows reports EXHAUSTED against an
#                                   empty chain.
#   deleted  payment_requests       deposit addresses and quotes denominated on
#                                   the dead chain; any observed balance is gone
#                                   with it.
#   deleted  writer_lease           expires on its own, but clearing it lets the
#                                   restarted writer claim immediately instead of
#                                   waiting out a lease from before the reset.
#
#   KEPT     app_attest_keys        per-install Apple device keys. No chain
#                                   relationship at all; deleting them makes
#                                   every tester reinstall to re-attest.
#   KEPT     registration_vouchers  minted voucher hashes. The plaintext existed
#                                   exactly once, in the mint CLI's stdout, so a
#                                   delete cannot be undone — the vouchers would
#                                   have to be re-minted and redistributed.
#   KEPT     auth_challenges,       TTL'd. They expire faster than anyone can
#            refresh_tokens         act on this script.
#
# The username-indexer database is NOT touched by default: since the
# `genesis_hash` guard in `crates/username-indexer/src/bootstrap.rs`, the
# service detects a changed chain at boot and rebuilds its projection itself.
# `--include-indexer` forces that rebuild by hand, for when the guard is not yet
# deployed or the projection needs clearing for some other reason.
#
# Usage:
#   scripts/reset_env_state.sh --project <name> --confirm <name> [--include-indexer] [--dry-run]
#
#   --project          compose project to act on (`identity-backend-v2` for the
#                      primary stack, `identity-previewnet` for PreviewNet).
#   --confirm          must repeat --project exactly. The whole guardrail: this
#                      command is destructive and both environments run from the
#                      same compose file on one host, so naming the target twice
#                      is what stops a reset landing on the wrong stack.
#   --include-indexer  also clear the username-indexer projection + checkpoint.
#   --dry-run          report the row counts that would be deleted, delete
#                      nothing, and leave every service running.
#
# Run from the checkout whose compose file defines the target project.

set -euo pipefail

project=""
confirm=""
include_indexer=false
dry_run=false

die() {
  echo "error: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project)
      [[ $# -ge 2 ]] || die "--project needs a value"
      project="$2"
      shift 2
      ;;
    --confirm)
      [[ $# -ge 2 ]] || die "--confirm needs a value"
      confirm="$2"
      shift 2
      ;;
    --include-indexer)
      include_indexer=true
      shift
      ;;
    --dry-run)
      dry_run=true
      shift
      ;;
    -h|--help)
      sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ -n "$project" ]] || die "--project is required"
[[ -n "$confirm" ]] || die "--confirm is required (repeat the project name)"
if [[ "$project" != "$confirm" ]]; then
  die "--confirm ($confirm) does not match --project ($project); refusing to touch either"
fi

compose() {
  docker compose -p "$project" "$@"
}

# A project with no containers is almost always a typo'd name or the wrong
# checkout — either way the deletes would land somewhere unintended, or nowhere.
if [[ -z "$(compose ps --all --quiet 2>/dev/null)" ]]; then
  die "compose project '$project' has no containers here; wrong name or wrong checkout?"
fi

# Identity database. `-T` because this runs unattended in a deploy shell.
identity_psql() {
  compose exec -T postgres psql -qAt -U identity -d identity -c "$1"
}

indexer_psql() {
  compose exec -T username-indexer-postgres \
    psql -qAt -U username_indexer -d username_indexer -c "$1"
}

count() {
  local table="$1"
  identity_psql "SELECT count(*) FROM $table" | tr -d '[:space:]'
}

echo "project:        $project"
echo "identity rows:  username_reservations=$(count username_reservations)" \
     "payment_requests=$(count payment_requests)" \
     "writer_lease=$(count writer_lease)"
echo "preserved:      app_attest_keys=$(count app_attest_keys)" \
     "registration_vouchers=$(count registration_vouchers)"

if [[ "$include_indexer" == true ]]; then
  indexer_rows="$(indexer_psql "SELECT count(*) FROM assigned_usernames" | tr -d '[:space:]')"
  echo "indexer rows:   assigned_usernames=$indexer_rows"
fi

if [[ "$dry_run" == true ]]; then
  echo
  echo "dry run: nothing deleted, no service stopped."
  exit 0
fi

# Stop the writers before deleting. The chain writer's claim scan and the queue
# advancer both mutate `username_reservations` continuously; deleting underneath
# them races a row into SUBMITTING after the delete and leaves a half-reset
# outbox. `stop` on a service the environment does not run (PreviewNet has no
# registration-queue) is a no-op, so both are unconditional.
echo
echo "stopping writers…"
compose stop identity-chain-writer registration-queue >/dev/null 2>&1 || true

deleted_reservations="$(identity_psql \
  "WITH gone AS (DELETE FROM username_reservations RETURNING 1) SELECT count(*) FROM gone" \
  | tr -d '[:space:]')"
deleted_payments="$(identity_psql \
  "WITH gone AS (DELETE FROM payment_requests RETURNING 1) SELECT count(*) FROM gone" \
  | tr -d '[:space:]')"
identity_psql "DELETE FROM writer_lease" >/dev/null

echo "deleted:        username_reservations=$deleted_reservations" \
     "payment_requests=$deleted_payments writer_lease=all"

if [[ "$include_indexer" == true ]]; then
  echo "stopping username-indexer…"
  compose stop username-indexer >/dev/null 2>&1 || true
  # Checkpoint first, then rows, in one statement: a projection left without its
  # checkpoint re-bootstraps, but rows deleted while the checkpoint survives
  # leave an indexer that resumes incrementally over an empty table and never
  # refills it.
  indexer_psql "BEGIN;
    DELETE FROM sync_state WHERE id = 1;
    DELETE FROM assigned_usernames;
    COMMIT;" >/dev/null
  echo "deleted:        assigned_usernames + sync_state (full re-bootstrap on next boot)"
fi

echo "restarting services…"
compose up -d >/dev/null

echo
echo "done. The writer re-registers from an empty outbox; the indexer rebuilds"
echo "its projection from the current chain. Re-check the on-chain prerequisites"
echo "(attester allowance, proxy delegation, signer funding) before expecting"
echo "registrations to land — see docs/plans/active/previewnet-env.md."
