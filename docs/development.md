# Development

## Prerequisites

Install mise and configure rootless Podman. On macOS, start a Podman machine
before running database tasks. On Linux, `podman info` should work without sudo.

Rust crates also need a C compiler, linker, `pkg-config` and OpenSSL headers.
On Debian/Ubuntu these are `build-essential pkg-config libssl-dev`; on Arch,
`base-devel openssl`. Mise manages Rust (including rustfmt, Clippy and the WASM
target), Trunk, Tailwind, Binaryen, watchexec, Python and uv. Iris tasks install
the pinned JDK on demand; the committed Gradle wrapper selects Gradle.

From the repository root:

```bash
mise trust
mise install
mise run dev
```

## Daily work

`mise run dev` runs the API and map. `mise run dev:full` adds claims and the Iris gateway.
The claims client rebuilds on changes; refresh its page after the build finishes.
Its route HTML comes from Axum, so Trunk browser autoreload is disabled there.
The map client reloads automatically.

Ctrl-C stops the native processes; PostgreSQL stays up until `mise run db:stop`.
Use `mise tasks` for the command list. Individual processes are `mise run server`,
`mise run client`, `mise run claims` and `mise run ingest`.

- Map: <http://127.0.0.1:8081>
- Claims: <http://127.0.0.1:8082/claims/new/blank>
- API: <http://127.0.0.1:3000/api/health>
- Gateway: <http://127.0.0.1:3010/health>

Both Trunk servers proxy `/api/` to port 3000 and watch their shared Rust sources.
The native API and gateway use watchexec to rebuild and restart after changes.
Release builds are explicit (`mise run build:client`, `mise run build:claims`,
`mise run build:server`, `mise run build:ingest`).

The API still binds to `0.0.0.0:3000` outside the dev task; `mise run server` sets
`SEQUOIA_SERVER_BIND=127.0.0.1:3000`. Local PostgreSQL, Trunk and the gateway also
bind to loopback. Development ingest credentials are public local defaults,
not production secrets. Set `INTERNAL_INGEST_TOKEN` to override the shared
API/gateway token.

`SEQUOIA_BACKEND_BASE_URL` enables backend-dependent features. Without it, live
territory data still works, but sign-in and the war-controller feed do not.
Do not add production credentials to `mise.toml`. Use exported variables or the
gitignored `mise.local.toml` for local configuration. Avoid real credentials
when running the Fabric development client or untrusted reporter builds.

## Local data

`mise run db:start` creates or starts `sequoia-map-dev-postgres`, using PostgreSQL 18
and the named volume `sequoia-map-dev-postgres-data`. `mise run db:status` checks
readiness; `mise run db:stop` stops it without removing data. `mise run db:url` prints
the development connection string.

The container exposes only `127.0.0.1:55432`. Set `SEQUOIA_PG_PORT` before first
creation to change the host port. Set `SEQUOIA_PG_CONTAINER` as well when running
independent checkouts side by side; each container gets its own named volume.
A pre-existing container retains the port it was created with.

Export `DATABASE_URL` to use an external development database; the server task
then skips Podman. The gateway stores its SQLite database in `.data/`.

The old `just`/`dev.sh` launchers, native PostgreSQL bootstrap and Docker hot-reload
stack have been replaced. Their `.data/postgres`, Docker containers and volumes
are not migrated or deleted by mise. Stop the old stack before using the same
ports. Export/import any history you need rather than mounting an old data
directory into the new container.

## Verification

```bash
mise run fmt
mise run verify
mise run lint
mise run iris:test
mise run iris:build 1.21.11
```

`mise run verify` checks formatting, all native Rust targets and both WASM clients;
runs both Rust workspaces' tests; builds both browser clients in release mode;
and enforces the map's compressed WASM budget. CI runs the same task. The
separately locked ingest workspace is included, rather than relying on the
root Cargo workspace to find it. Clippy rejects warnings on native, WASM and ingest
targets. Intentional argument-count exceptions use function-local `#[expect]`
with a reason; Clippy reports expectations that are no longer needed. Do not
add crate-wide warning suppression or change API shapes solely to silence lints.

`mise run test` starts the local container and creates **`sequoia_test`**, separate
from the development database. Some integration tests truncate tables.
To use external PostgreSQL in CI or locally, set `TEST_DATABASE_URL` to a
**disposable test database**. The task deliberately does not inherit
`DATABASE_URL` for tests. Running `cargo test` directly without a database skips
some database integration coverage and is not a substitute for `mise run test`.

The map size check optimizes a temporary copy of the release WASM, then measures
Brotli and gzip sizes. It does not overwrite Trunk's hashed assets or their
integrity metadata.

Iris uses Minecraft-specific dependency profiles and is tested separately; see
[the reporter guide](../mods/wynn-iris/README.md). `mise run iris:dev` launches the
Fabric development client. It does not install or overwrite mods in a personal
Minecraft instance. Java compiler warnings and Gradle deprecations fail the build.

## Browser smoke test

After starting `mise run dev:full` in another terminal:

```bash
mise run smoke:install  # one-time Chromium download
mise run smoke
```

The test opens the map and both claims entry points, waits for their canvases,
and fails on browser exceptions or failed HTTP requests. Screenshots and results
are written to `.data/browser-smoke/`. It uses software rendering, not a GPU
performance benchmark. Set `SEQUOIA_BROWSER_EXECUTABLE` to use an existing Chromium
binary instead of the Playwright download. Linux still needs Chromium's shared
libraries; Playwright reports missing host dependencies on startup.

For renderer changes, also compare the map and claims editor at the same data,
zoom and viewport: labels, resource icons, selection, panning and live/history
transitions. The smoke test detects startup and resource failures, not visual
parity. Use release builds on the same machine for performance comparisons.
Set `window.__SEQUOIA_GPU_DIAG__ = true` before renderer initialization for
`gpu-diag` rebuild counters; pan-only frames should normally reuse labels/icons.
Do not commit temporary diagnostic instrumentation.

## Containers

Podman is only required for the default local database. Native compilation
keeps hot reload and debugger access outside a development image. For production
images, use the existing Dockerfiles with `podman build` or Docker; see
[deployment](deployment.md). Database maintenance scripts under `ops/` target
production Docker Compose, not the local Podman container.
