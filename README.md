# Sequoia Map

[![Rust](https://img.shields.io/badge/Rust-2024_Edition-b7410e?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![Leptos](https://img.shields.io/badge/Leptos-0.7-ef3939?style=flat-square)](https://leptos.dev/)
[![wgpu](https://img.shields.io/badge/wgpu-24.0-4b8bbe?style=flat-square)](https://wgpu.rs/)
[![Axum](https://img.shields.io/badge/Axum-0.8-222222?style=flat-square)](https://github.com/tokio-rs/axum)
[![WebAssembly](https://img.shields.io/badge/WebAssembly-654ff0?style=flat-square&logo=webassembly&logoColor=white)](https://webassembly.org/)
[![Tailwind CSS](https://img.shields.io/badge/Tailwind_CSS-06b6d4?style=flat-square&logo=tailwindcss&logoColor=white)](https://tailwindcss.com/)
[![PostgreSQL](https://img.shields.io/badge/PostgreSQL-17-4169e1?style=flat-square&logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![Docker](https://img.shields.io/badge/Docker-2496ed?style=flat-square&logo=docker&logoColor=white)](https://www.docker.com/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)

A [Wynncraft](https://wynncraft.com/) guild territory map built entirely in Rust: server, client, and shared types, compiled to WebAssembly & GPU Accelerated *for reasons!!*. 


## Stack

| Layer | Crates |
|-------|-------------|
| **Server** | Axum 0.8, Tokio, SQLx 0.8, DashMap, Tower-HTTP |
| **Client** | Leptos 0.7 (CSR &rarr; WASM), wgpu 24.0 (WebGL), Tailwind CSS |
| **Shared** | Serde, Chrono, crc32fast |

## Quick Start

The comments in combination with the README.md should be enough for anyone to pick this up.

### Prerequisites

- [Rust](https://rustup.rs/) 1.86+
- `wasm32-unknown-unknown` target | `rustup target add wasm32-unknown-unknown`
- [Trunk](https://trunkrs.dev/) | `cargo install trunk --locked`
- A running PostgreSQL instance

### Development

```bash
# Start the client dev server 
cd client && trunk serve

# In another terminal, start the backend
DATABASE_URL="postgres://user:pass@localhost:5432/sequoia" cargo run -p sequoia-server
```

If `trunk serve` exits immediately with a `--no-color` parsing error, run it as:

```bash
cd client && NO_COLOR=true trunk serve
```

### Development (Docker Hot Reload)

Run the full dev stack (Postgres + server hot reload + ingest hot reload + client hot reload):

```bash
./dev.sh
```

- Client dev server: `http://localhost:8080`
- Server API: `http://localhost:3000/api/...`
- Ingest API: `http://localhost:3010` (`/health`, `/metrics`, `/v1/*`)
- Postgres (optional host access): `localhost:55432` (override with `POSTGRES_PORT`)
- `./dev.sh` creates `.env.dev.local` on first run with stable `POSTGRES_PASSWORD`, `INTERNAL_INGEST_TOKEN`, and a free `POSTGRES_PORT`, then reuses them on later runs.
- `./dev.sh` also pins the Docker Compose project name to `sequoia-map-mod-ingest`, so this repo always reuses the same local containers and volumes instead of creating a second stack under a different checkout name.
- Pass normal Compose args through the script when needed: `./dev.sh down`, `./dev.sh logs -f server`, `./dev.sh ps`
- Server reload: `cargo watch` (watches `server/` and `shared/`)
- Ingest reload: `cargo watch` (watches `services/sequoia-ingest/` and `shared/`)
- Client reload: Trunk watch/rebuild in `client/`
- `.env.dev.local` is ignored by git and can be edited if you need to pin a different local port or rotate credentials.
- Manual env exports still work if you prefer them:

```bash
POSTGRES_PASSWORD=replace-with-strong-db-password \
INTERNAL_INGEST_TOKEN=replace-with-long-random-token \
docker compose -f docker-compose.dev.yml up --build
```

- Compile speed defaults for dev containers:
  - shared native `CARGO_TARGET_DIR` between `server` + `ingest` to avoid duplicate dependency builds
  - `CARGO_PROFILE_DEV_DEBUG=0` and `RUSTFLAGS=-C debuginfo=0` for faster incremental compile/link
  - `CARGO_BUILD_JOBS=16` default (override higher/lower per machine)

### Docker Compose

```bash
POSTGRES_PASSWORD=replace-with-strong-db-password docker compose up --build
```

Production compose is TLS-first and now runs:
- Caddy reverse proxy (`80/443`) as the public edge
- Sequoia server and ingest on private container networking only (no direct host port exposure)
- `restart: unless-stopped` for long-running services
- default Docker log rotation (`json-file`, configurable size/count)
- automatic PostgreSQL backups to a dedicated `pgbackups` volume

Set DNS for your domains and provide environment variables before starting:

```bash
POSTGRES_PASSWORD=replace-with-strong-db-password \
INTERNAL_INGEST_TOKEN=replace-with-long-random-token \
MAP_DOMAIN=map.example.com \
IRIS_DOMAIN=iris.example.com \
ACME_EMAIL=ops@example.com \
docker compose up --build -d
```

Public routes:
- `https://$MAP_DOMAIN` -> `server:3000`
- `https://$IRIS_DOMAIN` -> `ingest:3010`

Security notes:
- `/api/internal/ingest/*` is blocked at the public edge proxy.
- Public edge routes block `/api/metrics`, `/metrics`, and `/iris/metrics`; scrape metrics over private service networking.
- Compose defaults `INGEST_SINGLE_REPORTER_MODE=false`; only enable it explicitly for controlled single-reporter deployments.
- Compose defaults `INGEST_DEGRADED_SINGLE_REPORTER_ENABLED=false`; set it to `true` explicitly only if single-reporter degraded updates are required.
- Compose defaults `INGEST_TRUSTED_PROXY_CIDRS` to loopback + RFC1918 private ranges for containerized edge proxies; set explicit edge proxy CIDRs in production to narrow trust as needed.
- For local development, use `docker-compose.dev.yml` (plain localhost HTTP endpoints).

For faster repeat builds/deploys (especially on Coolify/VPS), use BuildKit cache and avoid
rebuilding unless needed:

```bash
# First build (or after Dockerfile/dependency changes)
DOCKER_BUILDKIT=1 COMPOSE_DOCKER_CLI_BUILD=1 docker compose build
docker compose up -d

# Normal restart without rebuilding
docker compose up -d --no-build
```

### Coolify (Docker Compose)

Use the Coolify-specific stack file (includes an internal edge proxy on `8080`; no host `80`/`443` bindings):

```bash
docker-compose.coolify.yml
```

Required environment variables in Coolify:

- `POSTGRES_PASSWORD`
- `INTERNAL_INGEST_TOKEN`

Routing in Coolify should target the `edge` service (port `8080`) for your map domain.
The included edge config then routes:

- `/v1/*` -> `ingest:3010` (reporter enroll/heartbeat/report)
- `/iris/v1/*` -> `ingest:3010` (same as above, via `/iris` subdirectory)
- everything else -> `server:3000` (map UI + API)

This allows a single public domain such as `https://map.example.com` for both map and ingest.

Health checks:

- `edge`: `/api/health` (map server health through edge)
- optional direct checks:
  - `server`: `/api/health`
  - `ingest`: `/health`

Reporter base URL examples:

- direct: `https://map.example.com`
- subdirectory: `https://map.example.com/iris` (the edge proxy strips `/iris`)

## Architecture

Three-crate Cargo workspace:

```
sequoia-map/
  shared/   — Territory, tower, and event types shared between server & client
  server/   — Axum API server, Wynncraft poller, SSE broadcaster, PostgreSQL persistence
  client/   — Leptos CSR app compiled to WASM, wgpu canvas renderer, sidebar UI
```

## Font Renderer Modes

Map label rendering now has four modes in `Settings -> Font -> Font Renderer`:

- `Auto` (default): uses Firefox behavior on Firefox and Classic behavior elsewhere.
- `Classic`: always uses the stable Canvas2D classic layout.
- `Dynamic`: keeps the previous Auto behavior (GPU static labels on Firefox mobile, dynamic Canvas2D layout otherwise).
- `GPU`: enables the experimental full GPU map-overlay path (static labels + dynamic map text + resource icons).

Notes:

- `GPU` mode initializes dual glyph atlases (fill + halo) and an icon atlas lazily.
- `Auto`/`Classic` behavior remains unchanged; full-stack GPU rendering is gated behind `GPU` mode only.
- If GPU text/icon resources fail to initialize, map overlays fall back to Canvas2D rendering.


## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL connection string | *(required)* |
| `RUST_LOG` | Tracing filter directive | `info` |
| `DB_MAX_CONNECTIONS` | SQLx PostgreSQL pool max connections | `10` |
| `SSE_BROADCAST_BUFFER` | In-memory SSE broadcast channel capacity | `256` |
| `INTERNAL_INGEST_TOKEN` | Shared secret for ingest -> server internal routes (>=24 chars; placeholders rejected) | *(required for ingest)* |
| `API_BODY_LIMIT_BYTES` | Max request body size accepted by server routes | `2097152` |
| `MAX_INGEST_UPDATES_PER_REQUEST` | Max canonical territory updates accepted per internal ingest request | `1024` |
| `MAX_HISTORY_REPLAY_EVENTS` | Max historical events replayed in `/api/history/at` reconstruction | `20000` |
| `MAX_HISTORY_SR_SAMPLE_ROWS` | Max raw rows loaded for `/api/history/sr-samples` | `20000` |
| `SEQ_LIVE_HANDOFF_V1` | Enable sequence-aware live-state handoff | `true` |
| `GUILDS_ONLINE_CACHE_TTL_SECS` | Cache freshness threshold used by `/api/guilds/online` | `120` |
| `GUILDS_ONLINE_MAX_CONCURRENCY` | Max concurrent upstream guild fetches in `/api/guilds/online` | `8` |
| `MAP_DOMAIN` | Public HTTPS domain routed to Sequoia server by Caddy | `map.example.com` |
| `IRIS_DOMAIN` | Public HTTPS domain routed to ingest by Caddy | `iris.example.com` |
| `ACME_EMAIL` | Email used for ACME certificate registration in Caddy | *(empty)* |
| `INGEST_TRUSTED_PROXY_CIDRS` | Comma-separated trusted reverse proxy CIDRs for `X-Forwarded-For` | *(empty in service; prod/coolify compose defaults to loopback + RFC1918 private ranges)* |
| `INGEST_SINGLE_REPORTER_MODE` | Restrict active enrollment/reporting to one reporter identity at a time | `false` *(prod/coolify compose defaults to `false`)* |
| `INGEST_DEGRADED_SINGLE_REPORTER_ENABLED` | Allow single active reporter to emit degraded canonical updates without quorum | `false` *(prod/coolify compose defaults to `false`; dev compose defaults to `true`)* |
| `INGEST_API_BODY_LIMIT_BYTES` | Max request body size accepted by ingest routes | `2097152` |
| `INGEST_MAX_REPORTS_PER_BATCH` | Max territory updates accepted per reporter upload batch | `1024` |
| `DOCKER_LOG_MAX_SIZE` | Docker log max size before rotation (Compose) | `10m` |
| `DOCKER_LOG_MAX_FILE` | Docker log file count to retain (Compose) | `5` |
| `BACKUP_INTERVAL_HOURS` | Interval between automatic PostgreSQL backups | `6` |
| `BACKUP_RETENTION_DAYS` | Days to keep automatic PostgreSQL backups | `14` |

## PostgreSQL Backups

Automatic backups are created by the `postgres-backup` service and stored in the `pgbackups` volume.

Utility scripts:

```bash
# List backups currently stored in /backups
./ops/backup/list_backups.sh

# Trigger an immediate backup
./ops/backup/backup_now.sh

# Restore latest backup into current DB
./ops/backup/restore_backup.sh

# Restore a specific backup file path from /backups
./ops/backup/restore_backup.sh /backups/sequoia_20260224T230000Z.sql.gz
```

Important: backups are generated with `--clean --if-exists`, so restore will drop/recreate dumped objects in the current database. It does not drop the database itself.

## Monitoring And Alerting

- Health endpoint: `/api/health`
- Prometheus endpoint: `/api/metrics`
- Alert rules file: `ops/prometheus/alerts/sequoia-map-alerts.yml`

Prometheus metrics exposed by `/api/metrics`:

| Metric | Type | Meaning |
|--------|------|---------|
| `sequoia_territories` | gauge | Current number of territories in live snapshot |
| `sequoia_guild_cache_size` | gauge | Current guild cache size |
| `sequoia_history_available` | gauge (0/1) | Whether history storage is available |
| `sequoia_seq_live_handoff_v1_enabled` | gauge (0/1) | Whether seq handoff mode is enabled |
| `sequoia_live_state_requests_total` | counter | Total `/api/live/state` requests |
| `sequoia_persist_failures_total` | counter | Total update persistence failures |
| `sequoia_dropped_update_events_total` | counter | Total updates dropped before broadcast |
| `sequoia_persisted_update_events_total` | counter | Total updates persisted before broadcast |
| `sequoia_guilds_online_requests_total` | counter | Total `/api/guilds/online` requests |
| `sequoia_guilds_online_cache_hits_total` | counter | Total guild rows served from cache by `/api/guilds/online` |
| `sequoia_guilds_online_cache_misses_total` | counter | Total guild rows requiring upstream fetch in `/api/guilds/online` |
| `sequoia_guilds_online_upstream_errors_total` | counter | Total upstream failures while serving `/api/guilds/online` |

Predefined alerts in `ops/prometheus/alerts/sequoia-map-alerts.yml`:

- `SequoiaMapTargetDown`
- `SequoiaMapMetricsMissing`
- `SequoiaMapPersistFailures`
- `SequoiaMapDroppedUpdates`
- `SequoiaMapHistoryUnavailable`
- `SequoiaMapSeqLiveHandoffDisabled`
- `SequoiaMapLiveStateRequestSpike`

Coolify/VPS monitoring notes:

- Configure health checks against `/api/health` (server) and `/health` (ingest).
- Scrape metrics from private service addresses (`server:3000/api/metrics` and `ingest:3010/metrics`); public edge routes now block metrics paths.
- Mount or sync `ops/prometheus/alerts/sequoia-map-alerts.yml` into your Prometheus rules directory.
- Tune alert thresholds (`for:` windows and request-rate thresholds) to match production traffic.

## CI And Integration Tests

- GitHub Actions workflow: `.github/workflows/ci.yml`
- CI provisions PostgreSQL (`postgres:17-alpine`) and runs server/client checks plus server tests.
- Included a -Postgres integration test that verifies:
  - poller update persistence into `territory_events`
  - `/api/history/bounds`
  - `/api/history/events`
  - `/api/history/sr-samples`
  - `/api/history/at`
- Route integration tests also cover:
  - invalid history query params returning `400`
  - history pagination via `after_seq`
  - `/api/health` and `/api/metrics` response contract shape

Run locally with a real database:

```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5432/sequoia" \
  cargo test -p sequoia-server -- --nocapture
```

## Project Structure

```
.
├── Cargo.toml            # Workspace root
├── Dockerfile            # Multi-stage build (client WASM + server binary)
├── docker-compose.yml    # Production stack (Caddy TLS edge + server + ingest + PostgreSQL)
├── docker-compose.coolify.yml # Coolify stack (platform TLS edge + server + ingest + PostgreSQL)
├── client/
│   ├── index.html        # Trunk entry point
│   ├── input.css         # Tailwind CSS input
│   ├── Trunk.toml
│   └── src/
│       ├── main.rs       # WASM entry point
│       ├── app.rs        # Root Leptos component
│       ├── canvas.rs     # wgpu canvas & territory rendering
│       ├── sidebar.rs    # Territory list & details panel
│       ├── history.rs    # Historical data fetching
│       ├── playback.rs   # Timeline playback controls
│       ├── timeline.rs   # Timeline scrub bar
│       ├── minimap.rs    # Minimap overlay
│       ├── tower.rs      # Tower info rendering
│       ├── sse.rs        # SSE client connection
│       └── ...
├── server/
│   ├── src/
│   │   ├── main.rs       # Axum server bootstrap
│   │   ├── state.rs      # Shared app state (DashMap + broadcast)
│   │   ├── config.rs     # Environment configuration
│   │   ├── routes/       # HTTP & SSE route handlers
│   │   └── services/     # Wynncraft poller, DB persistence
│   └── ...
├── services/
│   └── sequoia-ingest/   # Iris ingest gateway service
├── mods/
│   └── wynn-iris/        # Fabric reporter mod
├── ops/
│   └── caddy/            # Caddy TLS edge config
└── shared/
    └── src/
        ├── lib.rs        # Re-exports
        ├── territory.rs  # Territory types
        ├── tower.rs      # Tower & resource types
        ├── colors.rs     # Guild color generation
        └── ...
```

## License

[MIT](LICENSE)
