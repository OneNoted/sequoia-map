# Wynn Iris

Fabric client mod for territory-only Wynncraft live reporting.

## Scope

- auto-enrolls to ingest (`/v1/enroll`) and stores rotating token locally
- scans advancement territory entries and extracts territory model data only
- queues submissions locally in-memory with retry/backoff
- sends heartbeat to refresh token and reporter liveness
- by default sends only current map/runtime data used by Sequoia map rendering
- optional legacy scrapers can be enabled per-field, but their payload is metadata-only and ignored by map logic

## Data Policy

- Phase 1 is public advancement-map territory data only
- no chat logs or player names are submitted
- per-field sharing toggles are available and default to `true`
- strict validity gate pauses reporting during AFK/invalid-world states and resumes only after stable recovery
- captures guild-menu season tooltip hints (`Captured Territories`, `SR per Hour`) when visible for scalar calibration

## Configuration

Config file:

- `config/wynn-iris.json`

Breaking change: this release performs a clean config rename to `config/wynn-iris.json`.
Older config files (`config/iris-reporter.json`, `config/sequoia-fabric-reporter.json`) are ignored.

Important fields:

- `ingestBaseUrl`
- default `ingestBaseUrl` is `https://map.seqwawa.com`
- `allowInsecureIngestHttp` (default `false`; when false, non-localhost `http://` ingest URLs are blocked)
- `shareOwner`
- `shareHeadquarters`
- `shareHeldResources`
- `shareProductionRates`
- `shareStorageCapacity`
- `shareDefenseTier`
- `shareTradingRoutes` (default `false`; sends route scrape metadata only)
- `shareLegacyCaptureSignals` (default `false`)
- `shareLegacyWarSignals` (default `false`)
- `autoUpdateEnabled` (default `true`; startup release checks enabled by default)
- `autoUpdateRepo` (default `OneNoted/sequoia-map`)
- `autoUpdateIncludePrerelease` (default `false`; stable releases only)
- `autoUpdateLastCheckAt` (set automatically)
- `autoUpdateLastResult` (set automatically)
- `autoUpdatePendingVersion` (set automatically)
- `autoUpdatePendingAssetUrl` (set automatically)
- `reporterId`
- `token`

## Commands

- `/iris status`
- `/iris scalar`
- `/iris toggles`
- `/iris toggle <field> <true|false>`
- `/iris set-base-url <url>`
- `/iris update status`
- `/iris update check`
- `/iris update apply`
- `/iris update auto <true|false>`
- `/iris privacy`
- `/iris help`
- `/ir <subcommand>` (short alias)
- `/irisreporter <subcommand>` (compat alias)

Supported `<field>` values:

- `owner`
- `headquarters`
- `held_resources`
- `production_rates`
- `storage_capacity`
- `defense_tier`
- `trading_routes`
- `legacy_capture_signals`
- `legacy_war_signals`

## Build

```bash
cd mods/wynn-iris
./gradlew --no-daemon build
```

## Live Reload Development

Use the bundled launcher to target PrismLauncher `Wynncraft A` by default:

```bash
cd mods/wynn-iris
./live-dev.sh
```

What this does:

- runs `gradle remapJar --continuous` in the background
- copies the latest built mod jar into:
  - `~/.local/share/PrismLauncher/instances/Wynncraft A/.minecraft/mods/wynn-iris-live.jar`
- launches Prism with `prismlauncher --launch "Wynncraft A"`

Notes:

- compile watcher output is written to `build/live-reload-compile.log`
- jar sync events are written to `build/live-reload-sync.log`
- new jar versions are copied automatically, but Fabric loads them on next game launch

Optional overrides:

- `PRISM_INSTANCE_ID="Wynncraft B" ./live-dev.sh`
- `PRISM_ROOT_DIR="/custom/prism/root" ./live-dev.sh`

Loom dev runtime fallback (with HotswapAgent):

```bash
cd mods/wynn-iris
./live-dev.sh --loom
```

Loom mode extras:

- auto-installs latest compatible Fabric builds for:
  - `Auth Me` (for `/authme` re-auth flows in dev)
  - `Wynntils` (latest stable release for current `minecraft_version`)
- auto-installs a performance bundle for Loom:
  - `Sodium`, `Lithium`, `FerriteCore`, `EntityCulling`, `ImmediatelyFast`
  - `Voxy` (latest compatible build; alpha currently for 1.21.11)
- installs into `run/mods/` before `runClient` starts
- requires `jq` to resolve Modrinth versions
- disable auto-install with `LOOM_INSTALL_WYNN_MODS=0 ./live-dev.sh --loom`
- disable performance bundle with `LOOM_INSTALL_PERF_MODS=0 ./live-dev.sh --loom`

Build a specific Minecraft target profile:

```bash
cd mods/wynn-iris
./build-target.sh 1.21.11
./build-target.sh 1.21.4
```

Profile definitions are in `profiles/`.

## Release Tags

- updater checks GitHub releases in `OneNoted/sequoia-map` by default
- stable tags must follow `iris-vX.Y.Z` (for example `iris-v0.1.0`)
- release assets must include versioned profile jars like:
  - `wynn-iris-mc1.21.11-<version>.jar`
  - `wynn-iris-mc1.21.4-<version>.jar`
- `-sources.jar` assets are ignored by updater checks

## Notes

- Default target is `minecraft_version=1.21.11`; `1.21.4` is also supported via profile build.
- updater apply replaces the active jar on disk and requires a full Minecraft restart
- Parser is standalone and does not depend on Wynntils internals.
- When validity gating is active, `/iris status` shows:
  - `data_validity` (`valid`, `paused_afk`, `paused_invalid_world`, `recovering`)
  - `pause_reason`, `paused_for`, and `resume_progress`
- `/iris status` includes `scalar_hint` when a recent guild-menu season tooltip was detected
