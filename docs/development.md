# Development

## Setup

Install [mise](https://mise.jdx.dev) and a C toolchain (`build-essential` on Debian/Ubuntu,
`base-devel` on Arch, the Xcode command line tools on macOS). Then, from a checkout or a
new worktree:

```bash
mise trust      # once per checkout: approve this mise.toml
mise run dev
```

Mise installs the pinned tools from `mise.toml` and `mise.lock`: Rust with the WASM
target, Tailwind, Binaryen, PostgreSQL, pitchfork and sccache. Nothing else is needed:
no Docker, Podman, Node, Python or global Cargo installs.

## Daily work

`mise run dev` builds the API, starts this worktree's daemons and prints their ports:

- `postgres`, PostgreSQL 18 with its data in `.data/postgres`, kept across stops;
- `api`, the server, which serves the map at `/`, the claims editor at `/claims/new`
  and the API, exactly as production does;
- `watch`, which rebuilds after each save and updates open pages.

`mise run dev:full` adds `ingest`, the Iris gateway. `mise run stop` stops every daemon
and `mise run logs` follows their output. `mise daemons status` and `mise daemons tui`
show their state. Ports start at 3000 (API), 3900 (watcher), 3010 (gateway) and 15432
(PostgreSQL) in a primary checkout; each linked worktree gets its own stable offset,
so worktrees can run side by side. `mise daemons urls` lists them.

What a save does:

| You edit | What happens |
|---|---|
| `client/input.css`, `claims-client/input.css` | the stylesheet is rebuilt and swapped in place; the page keeps its state |
| client Rust (`client`, `claims-client`, `browser-map`, `wasm`) | the affected clients' WASM is rebuilt and their pages reload |
| `server/`, `shared/` | the server is rebuilt; only a successful build restarts it, then pages reload |
| `services/sequoia-ingest/` | the gateway is rebuilt and restarted, when it is running |

A failed build leaves the last good version running and shows the error at the bottom
of the page as well as in `mise run logs`.

Dev WASM is built with Leptos's type-erased views (`--cfg erase_components`), which
turns a minute-long client rebuild into seconds. Release builds keep the typed views.

Development credentials are public local defaults, not production secrets. Use exported
variables or the gitignored `mise.local.toml` for anything else, including a different
port base (redeclare the daemon there). `SEQUOIA_BACKEND_BASE_URL` enables sign-in and
the war-controller feed; without it, live territory data still works.

## Builds and caches

Each worktree has its own `target/`, so its first-party crates and incremental state are
its own and never shared or copied. sccache (`RUSTC_WRAPPER`, set by Mise outside CI)
keeps compiled registry dependencies in one local cache, so a new worktree reuses them
instead of compiling them again. Incremental first-party crates are never cached by
sccache, so another worktree's edits cannot leak into your build. Builds outside Mise
work too, just without the shared cache. `sccache --show-stats` shows its hit rate.

`mise run build` makes the release clients (with wasm-opt, integrity, compressed copies
and the map's WASM size budget) and the release API and gateway. `cargo xtask web --release`
builds only the release clients, into `client/dist` and `claims-client/dist`, the layout the
image and client releases use. Dev builds (`cargo xtask web` and the watcher) go to
`.data/dev-web` instead, which the dev API serves through `SEQUOIA_CLIENT_ROOT`, so a release
build never replaces the pages you are working on. The client build, the watcher and the
release commands live in
`tools/xtask`, its own workspace, so tool changes do not change the application's lockfile.

## Checks

Run what the change touches:

| Change | Check |
|---|---|
| Rust, dependencies, or anything unclassified | `mise run verify` before merging; hosted CI runs only `mise run ci` |
| Stylesheet only (`mise run release:plan` says so) | view it in `mise run dev`, then build it with `mise run release:client` |
| The repository tool | `mise run lint:tools` and `mise run test:tools` |
| Workflows or the CI lane script | `mise run lint:ci` and `mise run test:ci` |
| A Dockerfile | qualify the image from a `build-only` Deploy Images run ([deployment](deployment.md#coolify)) |

`mise run verify` runs formatting, Clippy on native, WASM, gateway and tool targets,
every test, the CI lane tests and the release client build. The server tests that use
PostgreSQL read only `TEST_DATABASE_URL`; `mise run test:db` (part of `mise run test`)
recreates a `sequoia_test` database in this worktree's PostgreSQL for each run unless
you set `TEST_DATABASE_URL` to another disposable database. Development data is never
used for tests.

Hosted CI runs `mise run ci`: formatting, Clippy on every target and the server's tests
against a fresh database. The pull-request run cancels superseded revisions; a change
that touches only files the checks never read skips them (`scripts/ci-lane.sh`).

## Rendering diagnostics

For renderer changes, compare the map and claims editor at the same data, zoom and
viewport: labels, resource icons, selection, panning and live/history transitions. Set
`window.__SEQUOIA_GPU_DIAG__ = true` before renderer initialization for `gpu-diag`
rebuild counters; set `window.__SEQUOIA_RENDER_STATS__ = true` before the map mounts
for an overlay with draw calls, uploads and the layers each frame rebuilt. Touch
gestures need a touch device or emulated touch input. Use release builds on the same
machine for performance comparisons, and do not commit temporary instrumentation.

Map image exports (`/api/live/map.webp`) read tiles, fonts and icons from the served map
client: `.data/dev-web/client/dist` in development, `client/dist` in the image.

## Migrating from the old setup

The Podman database (`sequoia-map-dev-postgres` and its volume) and Trunk servers are
no longer used and are not deleted. To keep old local history, `pg_dump` it from the old
container and restore it into `mise run psql`.
