#!/usr/bin/env bash
set -euo pipefail

# This container is separate from the retired Docker and native dev clusters.
name="${SEQUOIA_PG_CONTAINER:-sequoia-map-dev-postgres}"
port="${SEQUOIA_PG_PORT:-55432}"
image="docker.io/library/postgres:18.3-alpine"

url() {
  printf 'postgres://sequoia:sequoia@127.0.0.1:%s/%s\n' "$port" "$1"
}

start() {
  if podman container exists "$name"; then
    podman start "$name" >/dev/null
  else
    podman run --detach --name "$name" \
      --publish "127.0.0.1:${port}:5432" \
      --env POSTGRES_USER=sequoia --env POSTGRES_PASSWORD=sequoia \
      --env POSTGRES_DB=sequoia \
      --volume "${name}-data:/var/lib/postgresql" \
      "$image" >/dev/null
  fi
  binding="$(podman port "$name" 5432/tcp)"
  if [[ "$binding" != "127.0.0.1:${port}" ]]; then
    echo "$name uses $binding, not 127.0.0.1:${port}. Set SEQUOIA_PG_PORT to match or use a new SEQUOIA_PG_CONTAINER." >&2
    exit 1
  fi
  # The image's temporary initialization server only accepts Unix sockets.
  for ((attempt = 0; attempt < 60; attempt++)); do
    if podman exec "$name" pg_isready -h 127.0.0.1 -U sequoia -d sequoia >/dev/null 2>&1; then
      return
    fi
    sleep 1
  done
  podman logs "$name" >&2
  echo "PostgreSQL did not become ready within 60 seconds." >&2
  exit 1
}

case "${1:-}" in
  start) start ;;
  stop) podman stop "$name" ;;
  status) podman exec "$name" pg_isready -h 127.0.0.1 -U sequoia -d sequoia ;;
  url) url sequoia ;;
  test-url)
    start
    # Test suites truncate tables; never reuse the development database.
    exists="$(podman exec "$name" psql -U sequoia -d postgres -Atc \
      "SELECT 1 FROM pg_database WHERE datname = 'sequoia_test'")"
    if [[ "$exists" != 1 ]]; then
      podman exec "$name" createdb -U sequoia sequoia_test
    fi
    url sequoia_test
    ;;
  *) echo "Usage: $0 {start|stop|status|url|test-url}" >&2; exit 2 ;;
esac
