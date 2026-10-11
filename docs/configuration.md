# Server configuration

`DATABASE_URL` is required. The server applies committed SQL migrations at
startup and exits unsuccessfully if `DATABASE_URL` is missing, the database
connection or migrations fail, or the listener cannot bind. This is not blanket
configuration validation: invalid tokens are treated as unset, and many malformed
numeric settings fall back to defaults.

## Backend integration

- `SEQUOIA_BACKEND_BASE_URL`: Sequoia backend origin for sign-in, season data and
  the war-controller feed. Unset disables the war-controller poller and leaves
  visitors signed out; `/api/auth/login` returns 503.
- `SEQUOIA_BACKEND_PUBLIC_BASE_URL`: browser-facing backend origin for redirects
  in split deployments. Defaults to `SEQUOIA_BACKEND_BASE_URL`.
- `SEQUOIA_BACKEND_WARCONTROLLER_PATH`: defaults to `/internal/warcontroller`.
- `SEQUOIA_BACKEND_INTERNAL_TOKEN`: bearer token for backend calls.
- `INTERNAL_INGEST_TOKEN`: shared secret for gateway-to-server ingestion.

Both tokens must contain at least 24 characters; the ingest token also rejects
known placeholders. Use separate secrets for these two integrations. The server
must remain behind an edge that blocks public access to `/api/internal/ingest/*`.

## Runtime settings

Defaults below are the server's defaults. Compose files may override them.
See [`server/src/config.rs`](../server/src/config.rs) for parsing, bounds and
settings not listed here.

| Variable | Purpose | Default |
| --- | --- | --- |
| `SEQUOIA_SERVER_BIND` | API listener | `0.0.0.0:3000` |
| `RUST_LOG` | Tracing filter | `info` |
| `DB_MAX_CONNECTIONS` | PostgreSQL pool limit | `10` |
| `WYNNCRAFT_TERRITORY_URL` | Territory ownership source | `https://api.wynncraft.com/v3/guild/list/territory` |
| `TERRITORY_POLL_INTERVAL_SECS` | Territory polling interval | `10` |
| `WARCONTROLLER_POLL_INTERVAL_SECS` | War-controller polling interval | `5` |
| `WARCONTROLLER_MAX_STALENESS_SECS` | Drop stale war-controller cache after this interval; `0` disables expiry | `60` |
| `SSE_BROADCAST_BUFFER` | Live-event broadcast capacity | `256` |
| `SEQ_LIVE_HANDOFF_V1` | Sequence-aware live-state handoff | `true` |
| `API_BODY_LIMIT_BYTES` | Request body limit | `2097152` |
| `MAX_INGEST_UPDATES_PER_REQUEST` | Canonical updates per ingest request | `1024` |
| `MAX_HISTORY_REPLAY_EVENTS` | Events replayed by history reconstruction | `20000` |
| `MAX_HISTORY_SR_SAMPLE_ROWS` | Raw rows loaded for SR history samples | `20000` |
| `TERRITORY_HISTORY_RETENTION_DAYS` | Territory event/snapshot retention | `365` |
| `SEASON_HISTORY_RETENTION_DAYS` | Scalar sample/guild observation retention | `365` |
| `GUILDS_ONLINE_CACHE_TTL_SECS` | Online-guild cache lifetime | `120` |
| `GUILDS_ONLINE_MAX_CONCURRENCY` | Concurrent upstream guild requests | `8` |
| `GUILD_SEASONS_CACHE_TTL_SECS` | Season-definition cache lifetime | `130` |
| `MAP_IMAGE_ASSETS_DIR` | Tiles, fonts and icons for map image exports | `client/dist` |

Coolify defaults territory-history retention to `36500` days to preserve imported
history; ordinary production Compose defaults to `365`. Confirm the deployed
value before importing old history.

## Map image export

`GET /api/live/map.webp` and `GET /api/live/map.png` render the public live
territory map. Only the public live snapshot is drawn: no Intel, player
positions or other signed-in overlays. `?width=` takes 800 to 4096 pixels and
defaults to 4096, about one pixel per block; the WebP is about 7 MB there.

Responses carry a strong `ETag` over the drawn content (ownership, headquarters,
connections and hold times to the minute), the deployed `SOURCE_COMMIT` and the
asset contents, so `If-None-Match` answers 304 only for identical bytes.
`X-Map-Seq` and `X-Map-Timestamp` describe the live snapshot and `X-Map-As-Of`
the minute hold times are measured from; all come from one snapshot read.
`Cache-Control` is `public, max-age=5`. Invalid widths answer 400, an empty
snapshot or a full render queue 503 with `Retry-After`, and render failures 500.

WebP exports up to 12 megapixels use libwebp's sharp YUV conversion, which keeps thin
saturated borders and legend swatches close to the lossless colours; the 4096-pixel
default keeps plain conversion to bound memory. The Discord `/map` command attaches
`?width=2657`, the widest export no taller than 4096 pixels: Discord's media proxy
serves at most 4096 pixels per side, so a taller attachment is only shown resampled.

One render runs at a time on the blocking pool behind a queue of four requests.
A render at the default width takes about 3 s and peaks about 270 MB above the
server's baseline, inside the 768 MB container limit; the latest image per format
and width is kept.

## Gateway and deployment settings

The [gateway guide](../services/sequoia-ingest/README.md) owns ingest configuration,
including session verification, quorum, rate limits and trusted proxy CIDRs.
The [deployment guide](deployment.md) covers domains, TLS, images and backups.
Local-only settings live in [development](development.md), not production Compose.
