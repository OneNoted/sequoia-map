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

The deployment workflow is restricted to `OneNoted/sequoia-map`'s `main` branch.
It compares against the last successful image-publication and Coolify-trigger
run, so failed, cancelled and no-op runs cannot hide outstanding image changes.
Unknown inputs or unavailable baseline history rebuild every image. Server,
ingest and edge candidates build on independent runners with SHA tags; only
after all selected builds succeed are their exact digests promoted to `main`
and Coolify triggered. Production workflows are serialized without cancelling
an active deployment, and a superseded source head cannot promote its images.
Unchanged images keep their existing revisions; a successful trigger is not
proof that Coolify has completed the rollout. Verify running image digests and
the real service health after deployment.

A manual run chooses its `release` input:

- **`promote`, the default.** It behaves like a push to main: build, promote to
  `main` and trigger Coolify.
- **`build-only`.** It builds every image for the chosen ref, `main` or any other
  branch, and pushes them as `sha-<commit>` candidates with the usual registry
  cache, including the server's size check. It skips the Coolify credential
  probe, promotes no `main` tag and never triggers Coolify.

Use `build-only` to pull and test the real production image, by its candidate
tag, before it is promoted. A build-only run is never a promotion baseline: the
next push still compares against the last run that actually promoted. A failed
build-only run on main makes the next push rebuild every image, as any failed run
does.

The published images use Docker Official Images from Amazon ECR Public
(`public.ecr.aws/docker/library`) for their Rust, Debian, Go and Caddy bases. This
avoids Docker Hub's shared anonymous pull limit on GitHub-hosted builders while
retaining the same upstream images and version tags. Each base is pinned to its
digest, so base-image security updates arrive through a reviewed digest change.
The CI PostgreSQL service instead pulls the official `postgres:18.3-alpine` from
Docker Hub: GitHub-hosted runners hit the ECR mirror's anonymous data limit while
initializing service containers.

Rust dependency builds are separate manifest/lockfile layers, reused through
the existing registry build cache even on fresh runners. First-party stub
artifacts are cleaned before copying real source, preventing Cargo from
mistaking old source timestamps for fresh binaries. Keep the stub manifest
and source lists aligned with workspace members and any future build scripts;
the separately locked ingest workspace has its own dependency layer. Client
tooling is independent of these manifests, and Trunk matches the Mise pin.
The parallel native/WASM stages each use the Dockerfile's two Cargo jobs, which
fit the 4-vCPU image runner. The value is fixed because it is part of the
dependency layers' cache key; local builds can override `CARGO_BUILD_JOBS` with
a Docker build argument.
After pushing a server candidate, the build job pulls it and checks the map
client against the compressed size budget. It also checks that the image's
`/app/backend-key` equals `scripts/release.py key` for the same commit. An
over-budget or mismatched candidate is never promoted.

`docker-compose.coolify.dev.yml` is the separate deployed development stack,
not the local hot-reload environment.

## Client releases

Both browser clients can change without rebuilding or restarting a service. The
server serves them from immutable releases in the `client-releases` volume
(`SEQUOIA_CLIENT_RELEASES`), and `scripts/release.py` publishes them:

```bash
mise run release:plan                         # what a change needs, from its paths
mise run release:client -- --from-target      # build a release on the live one
mise run release:activate -- <bundle> --dry-run
mise run release:activate -- <bundle>         # install, switch, check, or roll back
mise run release:status
mise run release:rollback
```

`SEQUOIA_RELEASE_EXEC` is the command prefix that runs a command in the server
container, such as `ssh <host> docker exec -i <server container>`.
`SEQUOIA_RELEASE_URL` is the public URL the activation checks. Every stage
appends a timed receipt to `.data/releases/receipts.jsonl`.

What each part guarantees:

- **Stylesheet changes reuse the live release.** When a change since the live
  release's commit is only `client/input.css` (plus paths that need nothing,
  such as docs), `release:client` reuses every other byte of that release. It
  compiles the stylesheet with the image's Tailwind version and rewrites only its
  link, integrity and compressed copies. Any other client change runs each Trunk
  build once.
- **Installs are checked before anything changes.** The server installs a bundle
  only when every file matches its manifest and every page integrity reference
  matches the release's own bytes. A release's id comes from its contents, so a
  retry installs to the same place.
- **Activation is a guarded swap.** It is an atomic link switch under a lock, and
  it fails if the live release is no longer the one the command read. It refuses
  a release built for a different backend key: a digest of `Cargo.toml`,
  `Cargo.lock`, `shared/` and `server/`, which the image build records. Such a
  change needs a server release. Activating the live release again changes
  nothing, and a failed check after activation rolls back.
- **Pages loaded earlier keep working.** The live release and the two previously
  active ones are kept. A hashed CSS, JS or WASM name the live release lacks is
  served from a retained one. A `?v=` tile, font or icon comes from the release
  with that asset version, and an unknown version is not cached as immutable.
  Manifests and other control files are never served.
- **Image deploys stay coherent with static releases.** On its first start, a new
  server image installs and activates its own clients. It also activates them
  when the live release was built for another backend. A restart of the same
  image keeps a later static release or rollback.

Deploy Images uses the same selector. A stylesheet-only change with an unchanged
backend rebuilds, promotes and restarts nothing; its run notes that the change
is published with `release:client` and `release:activate`. Because nothing is
promoted, the next image-building change also rebuilds the server with that
stylesheet. Hosted checks skip stylesheet-only changes, since no Rust source
reads `client/input.css`. Mixed, shared, configuration and unknown changes
select images as before.

Compose recreates only the services whose image or configuration changed, so a
server release leaves PostgreSQL, ingest and edge running. A platform that sets
a per-deploy variable such as `SOURCE_COMMIT` on every service recreates them
all, so give such variables a stable value. The server labels releases with the
commit recorded in the image (`/app/source-commit`), not with that variable.

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
