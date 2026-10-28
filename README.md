# Sequoia Map

A [Wynncraft](https://wynncraft.com/) guild territory map with live ownership,
war overlays, historical playback and a claims editor. The API uses Axum and
PostgreSQL; the browser apps use Leptos, WebAssembly and wgpu.

Inspired by [Wynnmap](http://wynnmap.zatzou.com/) and
[Avicia's map](https://www.avicia.info/map).

![Sequoia Map screenshot](https://github.com/user-attachments/assets/a5690c41-44b5-4a7d-b18c-df695ba448f4)

## Run locally

Install [mise](https://mise.jdx.dev/getting-started.html) and a
[C toolchain](docs/development.md#setup), then:

```bash
mise trust
mise run dev
```

`mise run dev` prints the ports; in a primary checkout the map is at
<http://127.0.0.1:3000> and the claims editor at <http://127.0.0.1:3000/claims/new>.
Saves rebuild and update the open pages. PostgreSQL, the API and the watcher run as this
worktree's Mise daemons; `mise run stop` stops them. `mise run dev:full` adds the Iris
ingest gateway. See [development](docs/development.md).

The map fetches public territory data from Wynncraft. Website sign-in, season
backend data and the war-controller feed need a separate Sequoia backend;
see [configuration](docs/configuration.md).

```bash
mise tasks           # available commands
mise run fmt         # format every Rust workspace
mise run verify      # every check: Clippy, all tests, CI lint and the release client build
```

## Repository layout

- `server/`: API, polling, live event stream and PostgreSQL history.
- `client/`: live and history map UI.
- `claims-client/`: claims editor.
- `browser-map/`: the interactive map both clients mount: camera input, tiles,
  live feed and the wgpu renderer.
- `shared/`: types and calculations shared by the API, clients and gateway.
- `wasm/`: UI-independent map engine (camera gestures, scene invalidation,
  layout math) shared by both clients and tested natively.
- `services/sequoia-ingest/`: separately locked Rust workspace for Iris reports.
- `tools/xtask/`: separately locked repository tool: client builds, the dev watcher and releases.
- `ops/`: deployment edge, monitoring, backups and database maintenance.

## Documentation

- [Development](docs/development.md): setup, the dev stack, caches and checks.
- [Configuration](docs/configuration.md): server settings and backend integration.
- [Deployment and operations](docs/deployment.md): Compose, Coolify, backups and monitoring.
- [Iris gateway](services/sequoia-ingest/README.md): enrollment, quorum and ingest settings.

## License

[MIT](LICENSE).
