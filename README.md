# Sequoia Map

A [Wynncraft](https://wynncraft.com/) guild territory map with live ownership,
war overlays, historical playback and a claims editor. The API uses Axum and
PostgreSQL; the browser apps use Leptos, WebAssembly and wgpu.

Inspired by [Wynnmap](http://wynnmap.zatzou.com/) and
[Avicia's map](https://www.avicia.info/map).

![Sequoia Map screenshot](https://github.com/user-attachments/assets/a5690c41-44b5-4a7d-b18c-df695ba448f4)

## Run locally

Install [mise](https://mise.jdx.dev/getting-started.html),
[rootless Podman](https://podman.io/docs/installation) and the
[system build dependencies](docs/development.md#prerequisites), then:

```bash
mise trust
mise install
mise run dev
```

Open <http://127.0.0.1:8081>. Mise runs the API and map with hot reload and
starts PostgreSQL in Podman. `mise run dev:full` also starts the claims editor at
<http://127.0.0.1:8082/claims-app/> and the Iris ingest gateway on port 3010.

The map fetches public territory data from Wynncraft. Website sign-in, season
backend data and the war-controller feed need a separate Sequoia backend;
see [configuration](docs/configuration.md).

```bash
mise tasks       # available commands
mise run fmt         # format both Rust workspaces
mise run verify      # checks, tests with PostgreSQL, release browser builds and size budget
mise run iris:test   # Fabric reporter tests; installs the pinned JDK on first use
mise run iris:build  # default Minecraft profile; pass another profile as an argument
```

## Repository layout

- `server/`: API, polling, live event stream and PostgreSQL history.
- `client/`: map UI and renderer.
- `claims-client/`: claims editor; reuses map modules from `client/`.
- `shared/`: types and calculations shared by the API, clients and gateway.
- `wasm/`: browser utilities shared by both clients.
- `services/sequoia-ingest/`: separately locked Rust workspace for Iris reports.
- `mods/wynn-iris/`: Fabric reporter and its Gradle wrapper.
- `ops/`: deployment edge, monitoring, backups and database maintenance.

## Documentation

- [Development](docs/development.md): tools, tasks, local data and testing.
- [Configuration](docs/configuration.md): server settings and backend integration.
- [Deployment and operations](docs/deployment.md): Compose, Coolify, backups and monitoring.
- [Iris gateway](services/sequoia-ingest/README.md): enrollment, quorum and ingest settings.
- [Iris reporter](mods/wynn-iris/README.md): Minecraft profiles and reporter behavior.

## License

[MIT](LICENSE).
