#!/usr/bin/env bash
# Restores a backup (the latest by default) into the deployed database; see backup_now.sh
# for SEQUOIA_COMPOSE_EXEC.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
read -r -a compose <<< "${SEQUOIA_COMPOSE_EXEC:-docker compose}"

BACKUP_PATH="${1:-latest}"

if [[ "$BACKUP_PATH" == "latest" ]]; then
  BACKUP_PATH="$("${compose[@]}" exec -T postgres-backup sh -s <<'SCRIPT'
ls -1t /backups/sequoia_*.sql.gz 2>/dev/null | head -n1
SCRIPT
)"
fi

if [[ -z "$BACKUP_PATH" ]]; then
  echo "No backup file found."
  exit 1
fi

echo "Restoring backup: $BACKUP_PATH"
echo "This restores into the current database and will drop/recreate dumped objects."

"${compose[@]}" exec -T postgres-backup gzip -dc "$BACKUP_PATH" \
  | "${compose[@]}" exec -T postgres psql -v ON_ERROR_STOP=1 -U sequoia -d sequoia

echo "Restore complete."
