# Deployment and operations

The production Compose files remain Docker-based. Mise's rootless Podman
container is a separate [local development database](development.md#local-data).
Run commands below from the repository root against the intended deployment.

## Standalone Compose

Set DNS and export `POSTGRES_PASSWORD`, `INTERNAL_INGEST_TOKEN`, `MAP_DOMAIN`,
`IRIS_DOMAIN` and `ACME_EMAIL`, then:

```bash
docker compose up --build -d
```

`docker-compose.yml` runs Caddy on ports 80/443, the API, the Iris gateway,
PostgreSQL 18 and automatic backups. API and ingest ports are private to the
container network. Caddy routes the map domain to `server:3000` and the Iris
domain to `ingest:3010`.

Normal restarts do not need a rebuild:

```bash
docker compose up -d --no-build
```

Production images can also be built locally with Podman:

```bash
podman build -t sequoia-map-server .
podman build -f services/sequoia-ingest/Dockerfile -t sequoia-map-ingest .
podman build -f ops/caddy/Dockerfile -t sequoia-map-edge .
```

## Coolify

Use `docker-compose.coolify.yml`. It requires `POSTGRES_PASSWORD` and
`INTERNAL_INGEST_TOKEN`. Route the public map domain to `edge:8080`; Coolify
terminates TLS. This stack does not bind host ports 80/443.

The internal edge sends `/v1/*` and `/iris/v1/*` to ingest, stripping `/iris`
for the latter. Other requests go to the map server. Reporter base URLs can
therefore be `https://map.example.com` or `https://map.example.com/iris`.

GitHub Actions publishes the `server`, `ingest` and `edge` images to GHCR.
The default image tag is `main`; `SEQUOIA_SERVER_IMAGE`, `SEQUOIA_INGEST_IMAGE`
and `SEQUOIA_EDGE_IMAGE` override each image. Configure registry credentials
in Coolify if the packages are private. Trigger deployment only after image
publication succeeds.

`docker-compose.coolify.dev.yml` is the separate deployed development stack,
not the local hot-reload environment.

## Network and ingest policy

The edge blocks `/api/internal/ingest/*` and public metrics paths. Keep those
restrictions when changing proxies. Scrape private service addresses instead.

Production Compose leaves single-reporter and degraded-single-reporter modes
disabled. The default origin quorum is one, allowing separate reporters behind
the same NAT to corroborate. Set `INGEST_QUORUM_MIN_DISTINCT_ORIGINS=2` if you
need cross-origin corroboration; it is capped to the reporter threshold.

Compose trusts loopback and RFC1918 proxies by default. Narrow
`INGEST_TRUSTED_PROXY_CIDRS` to the actual edge proxy networks where possible.
See [gateway configuration](../services/sequoia-ingest/README.md) and
[server configuration](configuration.md) before overriding these policies.

## Backups and restore

The `postgres-backup` service writes compressed logical backups to the
`pgbackups` volume every six hours and retains them for 14 days. Override with
`BACKUP_INTERVAL_HOURS` and `BACKUP_RETENTION_DAYS`.

```bash
./ops/backup/list_backups.sh
./ops/backup/backup_now.sh
```

**Restore replaces dumped objects in the target database.** Dumps use
`--clean --if-exists`; they do not drop the database itself. Confirm the target
stack and take a current backup before running either form:

```bash
./ops/backup/restore_backup.sh                             # latest backup
./ops/backup/restore_backup.sh /backups/<backup-file>.sql.gz
```

PostgreSQL 18 mounts `/var/lib/postgresql` with
`PGDATA=/var/lib/postgresql/18/docker`. Do not attach a PostgreSQL 17 data
directory directly. Existing 17 installations have a dedicated upgrade runbook:

```bash
./ops/postgres/precheck_17_to_18.sh
./ops/postgres/upgrade_17_to_18.sh
./ops/postgres/verify_18_postcutover.sh
```

Read these scripts before a cutover. The upgrade takes a logical backup and a
compressed volume snapshot under `ops/postgres/snapshots/`, attempts
`pg_upgrade` link mode and falls back to copy mode. It is not part of normal
application startup or the local mise workflow.

## Monitoring

- API health: `/api/health`; ingest health: `/health`.
- Private Prometheus targets: `server:3000/api/metrics` and `ingest:3010/metrics`.
- Alert rules: [`ops/prometheus/alerts/sequoia-map-alerts.yml`](../ops/prometheus/alerts/sequoia-map-alerts.yml).

The metrics endpoints describe their series. The alert rules cover target and
metrics availability, persistence failures, dropped updates, unavailable history,
disabled sequence handoff and request spikes. Load the rules into Prometheus
and tune thresholds for the deployment rather than copying a second metrics
catalog into this guide.

Compose uses restart policies and log rotation. `DOCKER_LOG_MAX_SIZE` defaults
to `10m`; `DOCKER_LOG_MAX_FILE` defaults to `5`.
