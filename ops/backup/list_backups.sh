#!/usr/bin/env bash
# Lists the backups in the postgres-backup service; see backup_now.sh for SEQUOIA_COMPOSE_EXEC.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
read -r -a compose <<< "${SEQUOIA_COMPOSE_EXEC:-docker compose}"

"${compose[@]}" exec -T postgres-backup sh -s <<'SCRIPT'
ls -lh /backups/sequoia_*.sql.gz 2>/dev/null || echo "No backups found in /backups"
SCRIPT
