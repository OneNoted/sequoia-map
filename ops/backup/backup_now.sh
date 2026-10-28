#!/usr/bin/env bash
# Takes an on-demand backup in the postgres-backup service. SEQUOIA_COMPOSE_EXEC is the
# `docker compose` prefix for the deployed project, as for `mise run release:services`;
# without it, the project started from this checkout's compose.yaml.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
read -r -a compose <<< "${SEQUOIA_COMPOSE_EXEC:-docker compose}"

echo "Creating on-demand PostgreSQL backup..."
"${compose[@]}" exec -T postgres-backup sh -s <<'SCRIPT'
set -e
ts=$(date -u +%Y%m%dT%H%M%SZ)
file="/backups/sequoia_${ts}.sql"
pg_dump -h "$POSTGRES_HOST" -p "$POSTGRES_PORT" -U "$POSTGRES_USER" -d "$POSTGRES_DB" --clean --if-exists --no-owner --no-privileges > "$file"
gzip -f "$file"
echo "[backup] wrote ${file}.gz"
SCRIPT
