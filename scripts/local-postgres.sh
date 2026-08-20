#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PG_ROOT="${SEQUOIA_PG_ROOT:-${ROOT_DIR}/.data/postgres}"
PGDATA="${SEQUOIA_PGDATA:-${PG_ROOT}/cluster}"
PGLOG="${SEQUOIA_PG_LOG:-${PG_ROOT}/postgres.log}"
PGSOCKETDIR="${SEQUOIA_PG_SOCKET_DIR:-${PG_ROOT}/socket}"
PGHOST="${SEQUOIA_PG_HOST:-127.0.0.1}"
PGPORT="${SEQUOIA_PG_PORT:-55432}"
PGSUPERUSER="${SEQUOIA_PG_SUPERUSER:-postgres}"
PGUSER="${SEQUOIA_PG_USER:-sequoia}"
PGDATABASE="${SEQUOIA_PG_DATABASE:-sequoia}"
PGPASSWORD="${SEQUOIA_PG_PASSWORD:-sequoia}"

usage() {
  cat <<'EOF'
Usage: scripts/local-postgres.sh <command>

Commands:
  start   Initialize and start the repo-local Postgres cluster
  stop    Stop the repo-local Postgres cluster
  status  Show cluster status
  url     Print the default DATABASE_URL
  env     Print shell-style environment variables for the local cluster
  reset   Stop and delete the repo-local Postgres cluster
EOF
}

require_bin() {
  local name="${1}"
  command -v "${name}" >/dev/null 2>&1 || {
    echo "${name} is required for native Postgres dev." >&2
    exit 1
  }
}

urlencode() {
  local value="${1}"
  local encoded=""
  local i byte
  local LC_ALL=C

  for ((i = 0; i < ${#value}; i++)); do
    byte="${value:i:1}"
    case "${byte}" in
      [a-zA-Z0-9.~_-])
        encoded+="${byte}"
        ;;
      *)
        printf -v encoded '%s%%%02X' "${encoded}" "'${byte}"
        ;;
    esac
  done

  printf '%s' "${encoded}"
}

database_url() {
  printf 'postgres://%s:%s@%s:%s/%s\n' \
    "$(urlencode "${PGUSER}")" \
    "$(urlencode "${PGPASSWORD}")" \
    "${PGHOST}" \
    "${PGPORT}" \
    "$(urlencode "${PGDATABASE}")"
}

psql_super() {
  psql \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGSUPERUSER}" \
    -d postgres \
    -v ON_ERROR_STOP=1 \
    "$@"
}

ensure_cluster_initialized() {
  if [[ -f "${PGDATA}/PG_VERSION" ]]; then
    return
  fi

  mkdir -p "${PG_ROOT}"
  initdb \
    -D "${PGDATA}" \
    --username="${PGSUPERUSER}" \
    --auth-host=trust \
    --auth-local=trust \
    --encoding=UTF8 \
    --no-instructions >/dev/null
}

cluster_running() {
  [[ -f "${PGDATA}/PG_VERSION" ]] && pg_ctl -D "${PGDATA}" status >/dev/null 2>&1
}

wait_until_ready() {
  local attempts=0
  until pg_isready -h "${PGHOST}" -p "${PGPORT}" -d postgres >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if (( attempts >= 40 )); then
      echo "Timed out waiting for repo-local Postgres on ${PGHOST}:${PGPORT}." >&2
      exit 1
    fi
    sleep 0.25
  done
}

ensure_role_and_database() {
  psql_super \
    -v db_user="${PGUSER}" \
    -v db_name="${PGDATABASE}" \
    -v db_password="${PGPASSWORD}" <<'SQL'
SELECT format('CREATE ROLE %I LOGIN SUPERUSER PASSWORD %L', :'db_user', :'db_password')
WHERE NOT EXISTS (
  SELECT 1 FROM pg_roles WHERE rolname = :'db_user'
)\gexec

SELECT format('ALTER ROLE %I WITH LOGIN SUPERUSER PASSWORD %L', :'db_user', :'db_password')\gexec

SELECT format('CREATE DATABASE %I OWNER %I', :'db_name', :'db_user')
WHERE NOT EXISTS (
  SELECT 1 FROM pg_database WHERE datname = :'db_name'
)\gexec
SQL
}

start_cluster() {
  require_bin initdb
  require_bin pg_ctl
  require_bin pg_isready
  require_bin psql

  ensure_cluster_initialized

  if ! cluster_running; then
    mkdir -p "${PG_ROOT}"
    mkdir -p "${PGSOCKETDIR}"
    pg_ctl \
      -D "${PGDATA}" \
      -l "${PGLOG}" \
      -o "-F -h ${PGHOST} -p ${PGPORT} -c unix_socket_directories=${PGSOCKETDIR}" \
      start >/dev/null
  fi

  wait_until_ready
  ensure_role_and_database

  printf 'Repo-local Postgres is ready: %s\n' "$(database_url)"
}

stop_cluster() {
  require_bin pg_ctl

  if cluster_running; then
    pg_ctl -D "${PGDATA}" stop -m fast >/dev/null
    echo "Repo-local Postgres stopped."
  else
    echo "Repo-local Postgres is not running."
  fi
}

status_cluster() {
  require_bin pg_ctl

  if [[ ! -f "${PGDATA}/PG_VERSION" ]]; then
    echo "Repo-local Postgres is not initialized."
    return 1
  fi

  if cluster_running; then
    echo "Repo-local Postgres is running on ${PGHOST}:${PGPORT}."
  else
    echo "Repo-local Postgres is initialized but stopped."
    return 1
  fi
}

reset_cluster() {
  if cluster_running; then
    stop_cluster
  fi
  rm -rf "${PG_ROOT}"
  echo "Repo-local Postgres data removed from ${PG_ROOT}."
}

print_env() {
  printf 'DATABASE_URL=%s\n' "$(database_url)"
  printf 'INTERNAL_INGEST_TOKEN=%s\n' "${SEQUOIA_INTERNAL_INGEST_TOKEN:-local-sequoia-internal-token-1234567890}"
}

main() {
  case "${1:-}" in
    start)
      start_cluster
      ;;
    stop)
      stop_cluster
      ;;
    status)
      status_cluster
      ;;
    url)
      database_url
      ;;
    env)
      print_env
      ;;
    reset)
      reset_cluster
      ;;
    *)
      usage >&2
      exit 1
      ;;
  esac
}

main "${@}"
