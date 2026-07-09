#!/usr/bin/env bash
# Manual fallback deploy from a local working copy (normal path is CI:
# .github/workflows/deploy.yml deploys automatically on push to main).
# Syncs the local tree (uncommitted changes included), then runs the same
# server-side script CI uses (backup -> build -> health check).
#
# Usage: scripts/deploy.sh
# Overrides: DEPLOY_HOST (default: ibv2), DEPLOY_URL (default: https://identity.dotspark.app)
set -euo pipefail

HOST="${DEPLOY_HOST:-ibv2}"
PUBLIC_URL="${DEPLOY_URL:-https://identity.dotspark.app}"
REMOTE_DIR='~/identity-backend-v2'

cd "$(dirname "$0")/.."

echo "==> Syncing local tree to $HOST:$REMOTE_DIR (server .env and .git are never touched)"
rsync -az --delete \
  --exclude='.git/' --exclude='target/' --exclude='.local/' --exclude='.zdx/' \
  --exclude='output/' --exclude='.playwright-cli/' --exclude='.polkadot/' \
  --exclude='.env' --exclude='.DS_Store' --exclude='*.log' \
  ./ "$HOST:$REMOTE_DIR/"

echo "==> Running server-side deploy"
ssh "$HOST" "bash $REMOTE_DIR/scripts/remote-deploy.sh"

echo "==> Public check: $PUBLIC_URL/readyz"
curl -fsS --max-time 20 "$PUBLIC_URL/readyz"
echo
