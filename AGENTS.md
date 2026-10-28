# Working in this repository

`mise run dev` starts this worktree's stack and `mise run stop` stops it. Match the
check to the change with the table in [docs/development.md](docs/development.md#checks);
`mise run verify` is the full gate for Rust or unclassified changes, and
`mise run release:plan` says whether a change is stylesheet-only
([Client releases](docs/deployment.md#client-releases)).

The ingest gateway and `tools/xtask` (client builds, the dev watcher, releases) each have
their own Cargo workspace and lockfile. Root-level Cargo commands alone do not check them.
Both browser clients and `browser-map` also need WASM-target checks; native builds use a GPU stand-in, not the real renderer.

Both browser clients mount the map from the `browser-map` crate (canvas, camera
input, tiles, live feed and the GPU renderer in `browser-map/src/gpu`); changes
there affect both binaries. The `wasm` crate (`sequoia-map-engine`) holds the
host-testable, UI-independent parts: camera gestures, scene invalidation, map
math and label layout.

Some PostgreSQL integration tests truncate tables. They read only `TEST_DATABASE_URL`,
which must point to disposable data; `mise run test:db` recreates one for each run.
Production maintenance scripts under `ops` are not local dev bootstrap commands.

The canonical repository is `OneNoted/sequoia-map`, which is also the reporter's
default update source. Workflows, image names and that default all use it.
