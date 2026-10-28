# Working in this repository

Use `mise tasks` for development commands. Match the check to the change
(`mise run release:plan` says whether it is stylesheet-only):

- Rust, dependency, mixed or unclassified changes: `mise run verify`, the full gate.
  Hosted CI runs only its `mise run ci` subset; run the full gate before merging.
- Stylesheet only: build with `mise run release:client -- --from-target`, reusing the live
  release's other bytes, and accept the changed CSS in a real browser
  ([Client releases](docs/deployment.md#client-releases)).
- Workflow or release tool only: `mise run lint:ci`, `test:ci` and `test:release`.
- Dockerfile only: the real candidate image from a `build-only` Deploy Images run
  ([Coolify](docs/deployment.md#coolify)).

Setup and local ports are in [docs/development.md](docs/development.md).

The ingest gateway has its own Cargo workspace and lockfile. Root-level Cargo
commands alone do not check it. Both browser clients and `browser-map` also need
WASM-target checks; native builds use a GPU stand-in, not the real renderer.

Both browser clients mount the map from the `browser-map` crate (canvas, camera
input, tiles, live feed and the GPU renderer in `browser-map/src/gpu`); changes
there affect both binaries. The `wasm` crate (`sequoia-map-engine`) holds the
host-testable, UI-independent parts: camera gestures, scene invalidation, map
math and label layout.

Some PostgreSQL integration tests truncate tables. `mise run test` chooses a
separate local test database; `TEST_DATABASE_URL` must point to disposable data.
Production maintenance scripts under `ops` are not local dev bootstrap commands.

The canonical repository is `OneNoted/sequoia-map`, which is also the reporter's
default update source. Workflows, image names and that default all use it.
