# Working in this repository

Use `mise tasks` for development commands and `mise run verify` for the Rust CI
checks. Setup and local ports are in [docs/development.md](docs/development.md).
The separate Fabric build is `mise run iris:build <profile>`.

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

The reporter's default update source is still `OneNoted/sequoia-map`, a separate
repository. Do not rewrite it to this repository merely to match the clone URL.
