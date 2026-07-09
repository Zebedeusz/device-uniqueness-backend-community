#!/usr/bin/env bash
# Runs ON the server (invoked by CI or scripts/deploy.sh after the code is updated).
# Steps: pre-deploy Postgres backup -> docker compose build/up -> wait for /readyz.
set -euo pipefail

KEEP_BACKUPS=14
cd "$(dirname "$0")/.."

echo "==> Backing up Postgres (keeps last $KEEP_BACKUPS)"
mkdir -p ~/backups
if sudo docker compose ps --status running postgres 2>/dev/null | grep -q postgres; then
  stamp=$(date +%Y%m%d-%H%M%S)
  sudo docker compose exec -T postgres pg_dump -U identity identity | gzip >~/backups/identity-"$stamp".sql.gz
  ls -1t ~/backups/identity-*.sql.gz | tail -n +$((KEEP_BACKUPS + 1)) | xargs -r rm --
  echo "    backup: ~/backups/identity-$stamp.sql.gz ($(du -h ~/backups/identity-"$stamp".sql.gz | cut -f1))"
else
  echo "    postgres not running — skipping backup (first deploy?)"
fi

echo "==> Building and starting containers (first build can take several minutes)"
sudo docker compose up -d --build

echo "==> Waiting for /readyz"
for i in $(seq 1 30); do
  if curl -fsS --max-time 5 http://127.0.0.1:8080/readyz; then
    echo
    echo "==> Deploy OK"
    sudo docker compose logs --no-color --since=5m identity-chain-writer | tail -n 5 || true
    exit 0
  fi
  [ "$i" -lt 30 ] && sleep 10
done

echo "ERROR: /readyz not ready after 5 minutes" >&2
sudo docker compose logs --no-color --since=10m identity-api identity-chain-writer | tail -n 40 >&2
exit 1
