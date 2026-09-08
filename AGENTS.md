# Working in this repository

Use `mise tasks` for development commands and `mise run verify` for the Rust CI
checks. Setup and local ports are in [docs/development.md](docs/development.md).
The separate Fabric build is `mise run iris:build <profile>`.

The ingest gateway has its own Cargo workspace and lockfile. Root-level Cargo
commands alone do not check it. Both browser clients also need WASM-target
checks; native tests use a shared GPU stand-in, not the real renderer.

`claims-client` includes source files from `client` by path. Changes to those
modules affect both binaries. The `wasm` crate contains host-testable math and
state helpers; the GPU implementation is in `client/src/gpu`.

Some PostgreSQL integration tests truncate tables. `mise run test` chooses a
separate local test database; `TEST_DATABASE_URL` must point to disposable data.
Production maintenance scripts under `ops` are not local dev bootstrap commands.

The reporter's default update source is still `OneNoted/sequoia-map`, a separate
repository. Do not rewrite it to this repository merely to match the clone URL.
