set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list --unsorted

check-native:
    @command -v cargo >/dev/null 2>&1 || { echo "cargo is required." >&2; exit 1; }
    @rustup target list --installed | grep -qx 'wasm32-unknown-unknown' || { echo "Install the wasm target with: rustup target add wasm32-unknown-unknown" >&2; exit 1; }
    @command -v trunk >/dev/null 2>&1 || { echo "trunk is required. Install it with: cargo install trunk --locked" >&2; exit 1; }
    @cargo watch --version >/dev/null 2>&1 || { echo "cargo-watch is required. Install it with: cargo install cargo-watch" >&2; exit 1; }

native-env:
    @./scripts/local-postgres.sh env

pg-url:
    @./scripts/local-postgres.sh url

pg-start:
    @./scripts/local-postgres.sh start

pg-stop:
    @./scripts/local-postgres.sh stop

pg-status:
    @./scripts/local-postgres.sh status

pg-reset:
    @./scripts/local-postgres.sh reset

server: check-native
    #!/usr/bin/env bash
    if [[ -z "${DATABASE_URL:-}" ]]; then
      ./scripts/local-postgres.sh start >/dev/null
      export DATABASE_URL="$(./scripts/local-postgres.sh url)"
    fi
    export INTERNAL_INGEST_TOKEN="${INTERNAL_INGEST_TOKEN:-local-sequoia-internal-token-1234567890}"
    export RUST_LOG="${RUST_LOG:-info}"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/dev-native}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-16}"
    export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
    export RUSTFLAGS="${RUSTFLAGS_NATIVE_DEV:-${RUSTFLAGS:--C debuginfo=0}}"
    cargo watch -w server -w shared -x 'run -p sequoia-server'

client: check-native
    #!/usr/bin/env bash
    cd client
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-../target/dev-wasm}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-16}"
    export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
    export RUSTFLAGS="${RUSTFLAGS_WASM_DEV:-${RUSTFLAGS:--C debuginfo=0}}"
    export NO_COLOR="${NO_COLOR:-true}"
    trunk serve \
      --config Trunk.dev.toml \
      --proxy-backend "${SEQUOIA_CLIENT_PROXY_BACKEND:-http://127.0.0.1:3000/api/}" \
      --proxy-rewrite /api/

claims-client: check-native
    #!/usr/bin/env bash
    cd claims-client
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-../target/dev-wasm}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-16}"
    export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
    export RUSTFLAGS="${RUSTFLAGS_WASM_DEV:-${RUSTFLAGS:--C debuginfo=0}}"
    export NO_COLOR="${NO_COLOR:-true}"
    trunk serve \
      --config Trunk.dev.toml \
      --proxy-backend "${SEQUOIA_CLAIMS_PROXY_BACKEND:-http://127.0.0.1:3000/api/}" \
      --proxy-rewrite /api/

ingest: check-native
    #!/usr/bin/env bash
    mkdir -p .data
    export INTERNAL_INGEST_TOKEN="${INTERNAL_INGEST_TOKEN:-local-sequoia-internal-token-1234567890}"
    export SEQUOIA_INTERNAL_INGEST_TOKEN="${SEQUOIA_INTERNAL_INGEST_TOKEN:-$INTERNAL_INGEST_TOKEN}"
    export SEQUOIA_SERVER_URL="${SEQUOIA_SERVER_URL:-http://127.0.0.1:3000}"
    export SEQUOIA_INGEST_BIND="${SEQUOIA_INGEST_BIND:-127.0.0.1:3010}"
    export SEQUOIA_INGEST_DB_URL="${SEQUOIA_INGEST_DB_URL:-sqlite://$PWD/.data/sequoia-ingest.db?mode=rwc}"
    export RUST_LOG="${RUST_LOG:-info}"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/dev-native}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-16}"
    export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
    export RUSTFLAGS="${RUSTFLAGS_NATIVE_DEV:-${RUSTFLAGS:--C debuginfo=0}}"
    cd services/sequoia-ingest
    cargo watch -w src -w Cargo.toml -w ../../shared -x run

dev: check-native
    #!/usr/bin/env bash
    if [[ -z "${DATABASE_URL:-}" ]]; then
      ./scripts/local-postgres.sh start >/dev/null
      export DATABASE_URL="$(./scripts/local-postgres.sh url)"
    fi
    pids=()
    cleanup() {
      trap - EXIT INT TERM
      for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
      done
      wait "${pids[@]}" 2>/dev/null || true
    }
    trap cleanup EXIT INT TERM

    just server &
    pids+=($!)
    just client &
    pids+=($!)

    printf '%s\n' \
      'Native minimal dev is up:' \
      '  map:    http://127.0.0.1:8081' \
      '  server: http://127.0.0.1:3000' \
      "  db:     ${DATABASE_URL}" \
      '' \
      'Stop with Ctrl+C.'

    wait -n "${pids[@]}"

dev-full: check-native
    #!/usr/bin/env bash
    if [[ -z "${DATABASE_URL:-}" ]]; then
      ./scripts/local-postgres.sh start >/dev/null
      export DATABASE_URL="$(./scripts/local-postgres.sh url)"
    fi
    pids=()
    cleanup() {
      trap - EXIT INT TERM
      for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
      done
      wait "${pids[@]}" 2>/dev/null || true
    }
    trap cleanup EXIT INT TERM

    just server &
    pids+=($!)
    just client &
    pids+=($!)
    just claims-client &
    pids+=($!)
    just ingest &
    pids+=($!)

    printf '%s\n' \
      'Native full dev is up:' \
      '  map:          http://127.0.0.1:8081' \
      '  claims:       http://127.0.0.1:8082/claims-app/' \
      '  server:       http://127.0.0.1:3000' \
      '  ingest:       http://127.0.0.1:3010' \
      "  db:           ${DATABASE_URL}" \
      '' \
      'Stop with Ctrl+C.'

    wait -n "${pids[@]}"
