# Sequoia Ingest Gateway

Standalone gateway for live reporter submissions (`/v1/*`) with:

- reporter enrollment + rotating bearer tokens
- SHA-256 token hashing at rest in SQLite (legacy plaintext tokens auto-migrated on startup)
- per-IP and per-reporter rate limits
- duplicate suppression + temporary quarantine for malformed spam
- quorum/degraded decisioning before canonical emit
- raw report persistence (SQLite) with retention purge
- async forwarding to Sequoia internal territory ingest route

## Data Policy

- Territory-only ingest in this phase
- No war/timer/tower collection route is active
- `guild_opt_in` fields are accepted as backward-compatible no-op inputs for one phase

## Run

```bash
cd services/sequoia-ingest
cargo run
```

Default bind: `0.0.0.0:3010`.

## Environment

- `SEQUOIA_INGEST_BIND` (default: `0.0.0.0:3010`)
- `SEQUOIA_INGEST_DB_URL` (default: `sqlite://./sequoia-ingest.db`)
- `SEQUOIA_SERVER_URL` (default: `http://127.0.0.1:3000`)
- `SEQUOIA_INTERNAL_INGEST_TOKEN` (or `INTERNAL_INGEST_TOKEN`) **required** (min 24 chars; placeholders are rejected)
- `INGEST_API_BODY_LIMIT_BYTES` (default: `2097152`)
- `INGEST_MAX_REPORTERS` (default: `10000`)
- `INGEST_RATE_LIMIT_IP_PER_MIN` (default: `300`)
- `INGEST_RATE_LIMIT_REPORTER_PER_MIN` (default: `120`)
- `INGEST_MAX_RATE_LIMIT_KEYS` (default: `20000`)
- `INGEST_QUORUM_MIN_REPORTERS` (default: `2`)
- `INGEST_DEGRADED_SINGLE_REPORTER_ENABLED` (default: `false`; compose stacks set `true` unless overridden)
- `INGEST_TRUSTED_PROXY_CIDRS` (default: empty; comma-separated CIDRs allowed to supply `X-Forwarded-For`)
- `INGEST_RAW_RETENTION_DAYS` (default: `7`)
- `INGEST_REPORTER_RETENTION_DAYS` (default: `30`)
- `INGEST_DUP_SUPPRESS_SECS` (default: `300`)
- `INGEST_MAX_SEEN_IDEMPOTENCY_KEYS` (default: `100000`)
- `INGEST_MAX_REPORTS_PER_BATCH` (default: `1024`)
- `INGEST_MAX_TERRITORY_NAME_LEN` (default: `96`)
- `INGEST_MAX_IDEMPOTENCY_KEY_LEN` (default: `128`)
- `INGEST_MALFORMED_THRESHOLD` (default: `8`)
- `INGEST_MAX_MALFORMED_PENALTY_KEYS` (default: `20000`)
- `INGEST_QUARANTINE_SECS` (default: `300`)
- `INGEST_MAX_PENDING_TERRITORIES` (default: `2048`)
- `INGEST_MAX_CLAIMS_PER_TERRITORY` (default: `64`)
- `INGEST_MAX_FORWARD_QUEUE` (default: `2048`)
- `INGEST_FORWARD_MAX_ATTEMPTS` (default: `10`)

## API

- `POST /v1/enroll`
- `POST /v1/report/territory`
- `POST /v1/heartbeat`
- `GET /health`
- `GET /metrics`

Reporter endpoints require `Authorization: Bearer <token>` (except `/v1/enroll`).

## Production Security Guidance

- Run ingest behind HTTPS termination (Caddy/Nginx/Traefik/etc.).
- Do not expose server internal ingest routes (`/api/internal/ingest/*`) publicly.
- Set `SEQUOIA_SERVER_URL` to a private/internal server address.
- Set a high-entropy `SEQUOIA_INTERNAL_INGEST_TOKEN` / `INTERNAL_INGEST_TOKEN`.
- Set `INGEST_DEGRADED_SINGLE_REPORTER_ENABLED=true` if single-reporter visibility is required; set it to `false` for strict multi-reporter quorum only.
- Configure `INGEST_TRUSTED_PROXY_CIDRS` to your proxy network ranges so client IP rate limits/quarantine use real origins.

## Reporter Field Toggles

The gateway stores and enforces per-reporter toggles for:

- owner
- headquarters
- held resources
- production rates
- storage capacity
- defense tier
- trading routes

Toggles are accepted on enroll/heartbeat and reflected back as the effective configuration.
