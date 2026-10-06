# Wynn Iris

A Fabric client mod that reports Wynncraft territory observations to the
[ingest gateway](../../services/sequoia-ingest/README.md). It enrolls automatically,
rotates its reporter token through heartbeats and retries queued submissions.
The parser does not depend on Wynntils internals.

## Data and credentials

Reports contain territory ownership and runtime fields from the advancement
map. Visible guild-menu season hints (`Captured Territories` and `SR per Hour`)
support scalar calibration. Reporting pauses in AFK or invalid-world states
and resumes after stable recovery.

Enrollment sends the Minecraft account UUID, username and session token to the
configured gateway. Heartbeats and report batches also carry a session refresh
token. These authenticate the account; territory-only reporting does **not**
mean anonymous enrollment. Only point the mod at a gateway you trust. Territory
reports do not contain chat logs. Optional legacy scrapers send metadata rather
than canonical map updates.

Configuration is stored in `config/wynn-iris.json`, including reporter tokens
and the device private key. Do not commit or share this file.

## Configuration and commands

`ingestBaseUrl` defaults to `https://map.seqwawa.com`.
`allowInsecureIngestHttp` defaults to `false`, blocking non-local HTTP gateways.
For local development, use `http://127.0.0.1:3010` with `mise run dev:full` from the
repository root.

Ownership, headquarters, held resources, production rates, storage capacity and
defense-tier sharing default to enabled. Trading routes, legacy capture signals
and legacy war signals default to disabled. Inspect the current configuration
with `/iris toggles`, then change a field with:

```text
/iris toggle <field> <true|false>
```

Fields: `owner`, `headquarters`, `held_resources`, `production_rates`,
`storage_capacity`, `defense_tier`, `trading_routes`, `legacy_capture_signals`,
`legacy_war_signals`.

Use `/iris status` for enrollment, upload and validity state, `/iris scalar` for
season hints, and `/iris set-base-url <url>` to change the gateway. `/iris help`
lists commands; `/ir` and `/irisreporter` are aliases. The stored settings and
defaults are defined in [`ReporterConfig.java`](src/main/java/io/iris/reporter/ReporterConfig.java).

## Build and test

From the repository root, after [mise setup](../../docs/development.md):

```bash
mise run iris:test
mise run iris:build 1.21.11
mise run iris:build 1.21.4
mise run iris:dev
```

Mise selects JDK 21 and invokes the committed Gradle wrapper. Gradle loads the
Minecraft, mappings, loader and Fabric API versions from `profiles/`; the default
profile is in `gradle.properties`. Profile builds run tests and write jars to
`mods/wynn-iris/build/libs/`. Each profile build cleans the previous output.

`mise run iris:dev` uses Loom's development client. It does not modify a personal
PrismLauncher instance or download an extra mod bundle. To test in a real
instance, copy the matching built jar yourself and restart Minecraft.

## Updates and releases

Automatic stable-release checks default to enabled. The configured release
repository is **`OneNoted/sequoia-map`**, which remains separate from this
repository; it is not a renamed URL. `autoUpdateRepo` changes that source.
Prereleases are excluded unless `autoUpdateIncludePrerelease` is enabled.

```text
/iris update status
/iris update check
/iris update apply
/iris update auto <true|false>
```

Updates require a signed manifest and a matching SHA-256 digest before install
or staging. Windows stages the replacement and uses a post-exit helper; other
platforms replace the jar directly for the next game launch.

The [release workflow](../../.github/workflows/iris-release.yml) uses tags such as
`iris-v0.1.3`. Releases need profile jars named `wynn-iris-mc<profile>-<version>.jar`
and both `iris-update-manifest.json` and `iris-update-manifest.sig`. Source jars
are ignored by the updater.

`IRIS_MANIFEST_SIGNING_KEY_PEM_B64` contains the Ed25519 private signing key
(PEM or base64-encoded PEM). The release workflow checks that its public key
matches `SIGNING_PUBLIC_KEY_BASE64_DER` in `IrisAutoUpdater.java`. Set up the
matching key pair before publishing a release; changing the download repository
does not change the trusted signing key.
