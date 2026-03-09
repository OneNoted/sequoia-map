use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use sequoia_shared::{GuildRef, Territory, TerritoryChange, TerritoryMap};
use sqlx::{Postgres, QueryBuilder};
use tracing::{info, warn};

use crate::config::{POLL_INTERVAL_SECS, WYNNCRAFT_TERRITORY_URL, canonical_override_ttl};
use crate::state::{
    AppState, ExtraTerrInfo, GuildColorMap, IngestTerritoryOverride, PreSerializedEvent,
    build_guild_color_lookup, lookup_guild_color,
};

type SequencedUpdates = Vec<(u64, TerritoryChange)>;
type PersistResultFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
type SerializedSnapshotPayloads = (Arc<Bytes>, Arc<Bytes>, Arc<Bytes>, Arc<Bytes>);
const UNCLAIMED_GUILD_UUID: &str = "00000000-0000-0000-0000-000000000000";
const UNCLAIMED_GUILD_NAME: &str = "Unclaimed";
const UNCLAIMED_GUILD_PREFIX: &str = "NONE";

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));
    let mut cached_extra: HashMap<String, ExtraTerrInfo> = HashMap::new();
    let mut cached_colors: GuildColorMap = HashMap::new();
    let mut cached_colors_normalized: GuildColorMap = HashMap::new();
    let override_ttl = canonical_override_ttl();

    loop {
        interval.tick().await;

        match fetch_territories(&state.http_client).await {
            Ok(mut new_map) => {
                let mut supplemental_changed = false;
                let cached_ingest_overrides = state.ingest_overrides.read().await.clone();

                // Refresh local cached supplemental data only when upstream fetchers mark it dirty.
                if state.extra_data_dirty.swap(false, Ordering::AcqRel) {
                    cached_extra = state.extra_terr.read().await.clone();
                    supplemental_changed = true;
                }
                if state.guild_colors_dirty.swap(false, Ordering::AcqRel) {
                    cached_colors = state.guild_colors.read().await.clone();
                    cached_colors_normalized = build_guild_color_lookup(&cached_colors);
                    supplemental_changed = true;
                }

                // Always merge from local caches so ownership changes don't drop supplemental fields.
                merge_supplemental_data(
                    &mut new_map,
                    &cached_extra,
                    &cached_colors,
                    &cached_colors_normalized,
                    &cached_ingest_overrides,
                    override_ttl,
                );

                process_polled_map(&state, new_map, supplemental_changed).await;
            }
            Err(e) => {
                warn!("Failed to fetch territories: {e}");
            }
        }
    }
}

fn merge_supplemental_data(
    new_map: &mut TerritoryMap,
    cached_extra: &HashMap<String, ExtraTerrInfo>,
    cached_colors: &GuildColorMap,
    cached_colors_normalized: &GuildColorMap,
    cached_ingest_overrides: &HashMap<String, IngestTerritoryOverride>,
    override_ttl: Duration,
) {
    let now = Utc::now();
    let ttl =
        chrono::Duration::from_std(override_ttl).unwrap_or_else(|_| chrono::Duration::seconds(180));

    for (name, terr) in new_map.iter_mut() {
        if let Some(info) = cached_extra.get(name) {
            terr.resources = info.resources.clone();
            terr.connections = info.connections.clone();
        }
        if let Some(override_info) = cached_ingest_overrides.get(name)
            && now.signed_duration_since(override_info.observed_at) <= ttl
        {
            if let Some(guild) = &override_info.guild {
                terr.guild = guild.clone();
            }
            if let Some(acquired) = override_info.acquired {
                terr.acquired = acquired;
            }
            if let Some(runtime) = &override_info.runtime {
                terr.runtime = Some(runtime.clone());
            }
        }
        // Re-apply canonical guild color after ingest ownership overrides.
        if terr.guild.color.is_none()
            && let Some(rgb) =
                lookup_guild_color(cached_colors, cached_colors_normalized, &terr.guild.name)
        {
            terr.guild.color = Some(rgb);
        }
    }
}

async fn process_polled_map(state: &AppState, new_map: TerritoryMap, supplemental_changed: bool) {
    process_polled_map_with(state, new_map, supplemental_changed, |pool, updates| {
        Box::pin(persist_updates(pool, updates))
    })
    .await;
}

async fn process_polled_map_with<F>(
    state: &AppState,
    new_map: TerritoryMap,
    supplemental_changed: bool,
    persist_updates_fn: F,
) where
    F: for<'a> FnOnce(&'a sqlx::PgPool, SequencedUpdates) -> PersistResultFuture<'a>,
{
    // 1. Read lock: compute diff, then release
    let (changes, has_removals, mut live_seq, mut live_timestamp) = {
        let current = state.live_snapshot.read().await;
        (
            compute_diff(&current.territories, &new_map),
            has_removed_territories(&current.territories, &new_map),
            current.seq,
            current.timestamp.clone(),
        )
    };

    if changes.is_empty() && !has_removals && !supplemental_changed {
        return;
    }

    let reserved_count = if has_removals {
        1_u64
    } else {
        match u64::try_from(changes.len()) {
            Ok(count) => count,
            Err(_) => {
                warn!("too many territory changes to reserve sequence range");
                return;
            }
        }
    };
    let mut seq_cursor = if reserved_count > 0 {
        match reserve_next_seq_block(state, reserved_count) {
            Some(base) => base,
            None => {
                warn!("sequence counter overflow while reserving sequence range");
                return;
            }
        }
    } else {
        live_seq
    };
    let mut outgoing = Vec::new();
    let mut sequenced_updates: SequencedUpdates = Vec::new();
    let emit_snapshot_event = has_removals || supplemental_changed;

    if has_removals {
        let Some(seq) = seq_cursor.checked_add(1) else {
            warn!("Sequence counter overflow while preparing snapshot event");
            return;
        };
        seq_cursor = seq;
        let timestamp = Utc::now().to_rfc3339();
        info!("territory set changed (removals detected), broadcasting snapshot");
        live_seq = seq;
        live_timestamp = timestamp;
    } else if !changes.is_empty() {
        let timestamp = Utc::now().to_rfc3339();
        info!("{} territory changes detected", changes.len());
        let mut update_build_failed = false;

        for change in changes {
            let Some(seq) = seq_cursor.checked_add(1) else {
                warn!("Sequence counter overflow while preparing update event");
                update_build_failed = true;
                break;
            };
            seq_cursor = seq;
            let update_json = match serialize_update_event(
                seq,
                std::slice::from_ref(&change),
                &timestamp,
                "update broadcast event",
            ) {
                Some(json) => json,
                None => {
                    update_build_failed = true;
                    break;
                }
            };
            live_seq = seq;
            live_timestamp = timestamp.clone();
            outgoing.push(PreSerializedEvent::Update {
                seq,
                json: update_json,
            });
            sequenced_updates.push((seq, change));
        }

        if update_build_failed {
            return;
        }
    }

    if !sequenced_updates.is_empty() {
        let sequenced_update_count = sequenced_updates.len() as u64;
        match state.db.as_ref() {
            Some(pool) => {
                if let Err(e) = persist_updates_fn(pool, sequenced_updates).await {
                    state.observability.record_persist_failure();
                    state
                        .observability
                        .record_dropped_update_events(sequenced_update_count);
                    warn!(
                        dropped_update_events = sequenced_update_count,
                        error = %e,
                        "failed to persist updates; continuing with in-memory live update"
                    );
                } else {
                    state
                        .observability
                        .record_persisted_update_events(sequenced_update_count);
                }
            }
            None => {
                state
                    .observability
                    .record_dropped_update_events(sequenced_update_count);
                warn!(
                    dropped_update_events = sequenced_update_count,
                    "database unavailable; continuing with in-memory live update only"
                );
            }
        }
    }

    let (snapshot_json, territories_json, live_state_json, ownership_json) =
        match serialize_all_formats(live_seq, &live_timestamp, &new_map) {
            Some(payloads) => payloads,
            None => return,
        };

    if emit_snapshot_event {
        outgoing.push(PreSerializedEvent::Snapshot {
            seq: live_seq,
            json: Arc::clone(&snapshot_json),
        });
    }

    {
        let mut current = state.live_snapshot.write().await;
        current.territories = new_map;
        current.snapshot_json = Arc::clone(&snapshot_json);
        current.territories_json = territories_json;
        current.live_state_json = live_state_json;
        current.ownership_json = ownership_json;
        current.seq = live_seq;
        current.timestamp = live_timestamp;
    }

    if seq_cursor > state.next_seq.load(Ordering::Relaxed) {
        state.next_seq.store(seq_cursor, Ordering::Relaxed);
    }

    for event in outgoing {
        let _ = state.event_tx.send(event);
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum SerializedUpdateEvent<'a> {
    Update {
        seq: u64,
        changes: &'a [TerritoryChange],
        timestamp: &'a str,
    },
}

fn serialize_update_event(
    seq: u64,
    changes: &[TerritoryChange],
    timestamp: &str,
    context: &str,
) -> Option<Arc<Bytes>> {
    match serde_json::to_vec(&SerializedUpdateEvent::Update {
        seq,
        changes,
        timestamp,
    }) {
        Ok(json) => Some(Arc::new(Bytes::from(json))),
        Err(e) => {
            warn!("failed to serialize {context}: {e}");
            None
        }
    }
}

fn reserve_next_seq_block(state: &AppState, count: u64) -> Option<u64> {
    if count == 0 {
        return Some(
            state
                .next_seq_reserved
                .load(Ordering::Relaxed)
                .max(state.next_seq.load(Ordering::Relaxed)),
        );
    }

    loop {
        let reserved = state.next_seq_reserved.load(Ordering::Relaxed);
        let committed = state.next_seq.load(Ordering::Relaxed);
        let base = reserved.max(committed);
        let next = base.checked_add(count)?;
        match state.next_seq_reserved.compare_exchange_weak(
            reserved,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(base),
            Err(_) => continue,
        }
    }
}

fn serialize_all_formats(
    seq: u64,
    timestamp: &str,
    territories: &TerritoryMap,
) -> Option<SerializedSnapshotPayloads> {
    #[derive(serde::Serialize)]
    struct OwnershipEntryRef<'a> {
        guild_uuid: &'a str,
        guild_name: &'a str,
        guild_prefix: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        guild_color: Option<(u8, u8, u8)>,
        acquired_at: &'a chrono::DateTime<chrono::Utc>,
    }

    let territories_vec = match serde_json::to_vec(territories) {
        Ok(json) => json,
        Err(e) => {
            warn!("failed to serialize live territory map: {e}");
            return None;
        }
    };
    let ownership_entries = territories
        .iter()
        .map(|(name, terr)| {
            (
                name.as_str(),
                OwnershipEntryRef {
                    guild_uuid: terr.guild.uuid.as_str(),
                    guild_name: terr.guild.name.as_str(),
                    guild_prefix: terr.guild.prefix.as_str(),
                    guild_color: terr.guild.color,
                    acquired_at: &terr.acquired,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let ownership_vec = match serde_json::to_vec(&ownership_entries) {
        Ok(json) => json,
        Err(e) => {
            warn!("failed to serialize ownership snapshot map: {e}");
            return None;
        }
    };

    let timestamp_json = match serde_json::to_string(timestamp) {
        Ok(json) => json,
        Err(e) => {
            warn!("failed to serialize live timestamp for payload wrappers: {e}");
            return None;
        }
    };

    let seq_json = seq.to_string();

    let territories_json = Arc::new(Bytes::from(territories_vec.clone()));

    let mut live_state_buf = Vec::with_capacity(territories_vec.len() + 96);
    live_state_buf.extend_from_slice(b"{\"seq\":");
    live_state_buf.extend_from_slice(seq_json.as_bytes());
    live_state_buf.extend_from_slice(b",\"timestamp\":");
    live_state_buf.extend_from_slice(timestamp_json.as_bytes());
    live_state_buf.extend_from_slice(b",\"territories\":");
    live_state_buf.extend_from_slice(&territories_vec);
    live_state_buf.push(b'}');

    let mut snapshot_buf = Vec::with_capacity(territories_vec.len() + 112);
    snapshot_buf.extend_from_slice(b"{\"type\":\"Snapshot\",\"seq\":");
    snapshot_buf.extend_from_slice(seq_json.as_bytes());
    snapshot_buf.extend_from_slice(b",\"territories\":");
    snapshot_buf.extend_from_slice(&territories_vec);
    snapshot_buf.extend_from_slice(b",\"timestamp\":");
    snapshot_buf.extend_from_slice(timestamp_json.as_bytes());
    snapshot_buf.push(b'}');

    Some((
        Arc::new(Bytes::from(snapshot_buf)),
        territories_json,
        Arc::new(Bytes::from(live_state_buf)),
        Arc::new(Bytes::from(ownership_vec)),
    ))
}

async fn persist_updates(
    pool: &sqlx::PgPool,
    sequenced_updates: SequencedUpdates,
) -> Result<(), String> {
    if sequenced_updates.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("begin transaction: {e}"))?;

    struct PersistInsertRow {
        stream_seq: i64,
        acquired_at: chrono::DateTime<chrono::Utc>,
        territory: String,
        guild_uuid: String,
        guild_name: String,
        guild_prefix: String,
        guild_color_r: Option<i16>,
        guild_color_g: Option<i16>,
        guild_color_b: Option<i16>,
        prev_guild_uuid: Option<String>,
        prev_guild_name: Option<String>,
        prev_guild_prefix: Option<String>,
        prev_guild_color_r: Option<i16>,
        prev_guild_color_g: Option<i16>,
        prev_guild_color_b: Option<i16>,
    }

    let mut rows = Vec::with_capacity(sequenced_updates.len());
    for (seq, change) in sequenced_updates {
        let stream_seq =
            i64::try_from(seq).map_err(|_| format!("sequence {seq} is out of i64 range"))?;

        let acquired_at = chrono::DateTime::parse_from_rfc3339(&change.acquired)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| {
                warn!(
                    "invalid acquired timestamp for territory {} at seq {}; using now()",
                    change.territory, seq
                );
                chrono::Utc::now()
            });

        let (guild_color_r, guild_color_g, guild_color_b) = split_color(change.guild.color);
        let (
            prev_guild_uuid,
            prev_guild_name,
            prev_guild_prefix,
            prev_guild_color_r,
            prev_guild_color_g,
            prev_guild_color_b,
        ) = match change.previous_guild {
            Some(g) => {
                let (r, gch, b) = split_color(g.color);
                (Some(g.uuid), Some(g.name), Some(g.prefix), r, gch, b)
            }
            None => (None, None, None, None, None, None),
        };

        rows.push(PersistInsertRow {
            stream_seq,
            acquired_at,
            territory: change.territory,
            guild_uuid: change.guild.uuid,
            guild_name: change.guild.name,
            guild_prefix: change.guild.prefix,
            guild_color_r,
            guild_color_g,
            guild_color_b,
            prev_guild_uuid,
            prev_guild_name,
            prev_guild_prefix,
            prev_guild_color_r,
            prev_guild_color_g,
            prev_guild_color_b,
        });
    }

    let mut query_builder = QueryBuilder::<Postgres>::new(
        "INSERT INTO territory_events \
         (stream_seq, acquired_at, territory, guild_uuid, guild_name, guild_prefix, \
          guild_color_r, guild_color_g, guild_color_b, prev_guild_uuid, prev_guild_name, \
          prev_guild_prefix, prev_guild_color_r, prev_guild_color_g, prev_guild_color_b) ",
    );
    query_builder.push_values(rows, |mut builder, row| {
        builder
            .push_bind(row.stream_seq)
            .push_bind(row.acquired_at)
            .push_bind(row.territory)
            .push_bind(row.guild_uuid)
            .push_bind(row.guild_name)
            .push_bind(row.guild_prefix)
            .push_bind(row.guild_color_r)
            .push_bind(row.guild_color_g)
            .push_bind(row.guild_color_b)
            .push_bind(row.prev_guild_uuid)
            .push_bind(row.prev_guild_name)
            .push_bind(row.prev_guild_prefix)
            .push_bind(row.prev_guild_color_r)
            .push_bind(row.prev_guild_color_g)
            .push_bind(row.prev_guild_color_b);
    });
    query_builder
        .build()
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("bulk insert territory updates: {e}"))?;

    tx.commit()
        .await
        .map_err(|e| format!("commit transaction: {e}"))?;
    Ok(())
}

fn split_color(color: Option<(u8, u8, u8)>) -> (Option<i16>, Option<i16>, Option<i16>) {
    match color {
        Some((r, g, b)) => (Some(i16::from(r)), Some(i16::from(g)), Some(i16::from(b))),
        None => (None, None, None),
    }
}

async fn fetch_territories(client: &reqwest::Client) -> Result<TerritoryMap, String> {
    let resp = client
        .get(WYNNCRAFT_TERRITORY_URL)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("failed to read response body: {e}"))?;

    if !status.is_success() {
        let preview = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>();
        return Err(format!("upstream status {status}; body preview: {preview}"));
    }

    parse_wynncraft_territory_payload(bytes.as_ref()).map_err(|e| {
        let preview = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>();
        format!("failed to decode territory payload: {e}; body preview: {preview}")
    })
}

#[derive(serde::Deserialize)]
struct RawGuildRef {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawTerritory {
    #[serde(default)]
    guild: Option<RawGuildRef>,
    acquired: chrono::DateTime<chrono::Utc>,
    location: sequoia_shared::Region,
    #[serde(default)]
    resources: sequoia_shared::Resources,
    #[serde(default)]
    connections: Vec<String>,
}

impl From<RawTerritory> for Territory {
    fn from(value: RawTerritory) -> Self {
        let guild = value.guild.unwrap_or(RawGuildRef {
            uuid: None,
            name: None,
            prefix: None,
        });
        let guild_name = guild
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(UNCLAIMED_GUILD_NAME)
            .to_string();
        let guild_prefix = guild
            .prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .unwrap_or(UNCLAIMED_GUILD_PREFIX)
            .to_string();
        let guild_uuid = guild
            .uuid
            .as_deref()
            .map(str::trim)
            .filter(|uuid| !uuid.is_empty())
            .unwrap_or(UNCLAIMED_GUILD_UUID)
            .to_string();

        Self {
            guild: GuildRef {
                uuid: guild_uuid,
                name: guild_name,
                prefix: guild_prefix,
                color: None,
            },
            acquired: value.acquired,
            location: value.location,
            resources: value.resources,
            connections: value.connections,
            runtime: None,
        }
    }
}

fn parse_wynncraft_territory_payload(bytes: &[u8]) -> Result<TerritoryMap, serde_json::Error> {
    let raw_map: HashMap<String, RawTerritory> = serde_json::from_slice(bytes)?;
    Ok(raw_map
        .into_iter()
        .map(|(territory, raw)| (territory, Territory::from(raw)))
        .collect())
}

fn compute_diff(old: &TerritoryMap, new: &TerritoryMap) -> Vec<TerritoryChange> {
    let mut changes = Vec::new();

    for (name, new_territory) in new {
        let changed = match old.get(name) {
            Some(old_territory) => {
                old_territory.guild.uuid != new_territory.guild.uuid
                    || old_territory.guild.name != new_territory.guild.name
                    || old_territory.guild.prefix != new_territory.guild.prefix
                    || old_territory.acquired != new_territory.acquired
                    || old_territory.runtime != new_territory.runtime
            }
            None => true, // new territory
        };

        if changed {
            let previous_guild = old.get(name).map(|t| GuildRef {
                uuid: t.guild.uuid.clone(),
                name: t.guild.name.clone(),
                prefix: t.guild.prefix.clone(),
                color: t.guild.color,
            });

            changes.push(TerritoryChange {
                territory: name.clone(),
                guild: GuildRef {
                    uuid: new_territory.guild.uuid.clone(),
                    name: new_territory.guild.name.clone(),
                    prefix: new_territory.guild.prefix.clone(),
                    color: new_territory.guild.color,
                },
                previous_guild,
                acquired: new_territory.acquired.to_rfc3339(),
                location: new_territory.location.clone(),
                resources: new_territory.resources.clone(),
                connections: new_territory.connections.clone(),
                runtime: new_territory.runtime.clone(),
            });
        }
    }

    changes
}

fn has_removed_territories(old: &TerritoryMap, new: &TerritoryMap) -> bool {
    old.keys().any(|name| !new.contains_key(name))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::time::Duration;

    use super::{
        UNCLAIMED_GUILD_NAME, UNCLAIMED_GUILD_PREFIX, UNCLAIMED_GUILD_UUID, compute_diff,
        has_removed_territories, merge_supplemental_data, parse_wynncraft_territory_payload,
        process_polled_map_with,
    };
    use axum::Router;
    use chrono::{DateTime, Utc};
    use sequoia_shared::history::{HistoryBounds, HistoryEvents, HistorySnapshot};
    use sequoia_shared::{GuildRef, Region, Territory, TerritoryMap, TerritoryRuntimeData};
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::oneshot;

    use crate::state::{AppState, PreSerializedEvent};

    fn territory(guild_uuid: &str, guild_name: &str, guild_prefix: &str) -> Territory {
        let acquired = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("fixed test timestamp should parse")
            .with_timezone(&Utc);
        Territory {
            guild: GuildRef {
                uuid: guild_uuid.to_string(),
                name: guild_name.to_string(),
                prefix: guild_prefix.to_string(),
                color: None,
            },
            acquired,
            location: Region {
                start: [0, 0],
                end: [10, 10],
            },
            resources: Default::default(),
            connections: Vec::new(),
            runtime: None,
        }
    }

    fn single_territory_map(
        guild_uuid: &str,
        guild_name: &str,
        guild_prefix: &str,
    ) -> TerritoryMap {
        let mut map = TerritoryMap::new();
        map.insert(
            "Alpha".to_string(),
            territory(guild_uuid, guild_name, guild_prefix),
        );
        map
    }

    fn lazy_test_pool() -> sqlx::PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://sequoia:sequoia@localhost/sequoia")
            .expect("lazy test pool should parse")
    }

    fn history_test_app(state: AppState) -> Router {
        crate::app::build_app(state)
    }

    async fn spawn_test_server(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        (addr, handle)
    }

    #[test]
    fn compute_diff_reports_new_and_changed_territories() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = TerritoryMap::new();
        new.insert("Alpha".to_string(), territory("g2", "GuildTwo", "G2"));
        new.insert("Beta".to_string(), territory("g3", "GuildThree", "G3"));

        let mut diff = compute_diff(&old, &new);
        diff.sort_by(|a, b| a.territory.cmp(&b.territory));

        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].territory, "Alpha");
        assert_eq!(diff[0].guild.uuid, "g2");
        assert_eq!(
            diff[0]
                .previous_guild
                .as_ref()
                .map(|guild| guild.uuid.as_str()),
            Some("g1")
        );
        assert_eq!(diff[1].territory, "Beta");
        assert!(diff[1].previous_guild.is_none());
    }

    #[test]
    fn compute_diff_skips_unchanged_owners() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = TerritoryMap::new();
        new.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let diff = compute_diff(&old, &new);
        assert!(diff.is_empty());
    }

    #[test]
    fn compute_diff_detects_same_uuid_guild_metadata_changes() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = TerritoryMap::new();
        new.insert("Alpha".to_string(), territory("g1", "GuildRenamed", "GRN"));

        let diff = compute_diff(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].territory, "Alpha");
        assert_eq!(diff[0].guild.uuid, "g1");
        assert_eq!(diff[0].guild.name, "GuildRenamed");
        assert_eq!(diff[0].guild.prefix, "GRN");
        assert_eq!(
            diff[0]
                .previous_guild
                .as_ref()
                .map(|guild| guild.name.as_str()),
            Some("GuildOne")
        );
    }

    #[test]
    fn compute_diff_detects_acquired_changes_without_owner_changes() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = old.clone();
        new.get_mut("Alpha").expect("alpha should exist").acquired =
            DateTime::parse_from_rfc3339("2026-01-01T00:00:10Z")
                .expect("valid timestamp")
                .with_timezone(&Utc);

        let diff = compute_diff(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].territory, "Alpha");
        assert_eq!(diff[0].guild.uuid, "g1");
    }

    #[test]
    fn compute_diff_detects_runtime_changes_without_owner_changes() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = old.clone();
        new.get_mut("Alpha").expect("alpha should exist").runtime = Some(TerritoryRuntimeData {
            contested: Some(true),
            ..TerritoryRuntimeData::default()
        });

        let diff = compute_diff(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].territory, "Alpha");
        assert_eq!(diff[0].guild.uuid, "g1");
    }

    #[test]
    fn removed_territories_detection_is_correct() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));
        old.insert("Beta".to_string(), territory("g2", "GuildTwo", "G2"));

        let mut new = TerritoryMap::new();
        new.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        assert!(has_removed_territories(&old, &new));

        new.insert("Beta".to_string(), territory("g2", "GuildTwo", "G2"));
        assert!(!has_removed_territories(&old, &new));
    }

    #[test]
    fn parse_wynncraft_payload_tolerates_null_guild_fields() {
        let payload = r#"{
            "Lion Lair": {
                "guild": {"uuid": null, "name": null, "prefix": null},
                "acquired": "2026-02-26T22:13:13.493000Z",
                "location": {"start":[890,-2140],"end":[790,-2320]}
            },
            "Ragni": {
                "guild": {"uuid": "abc", "name": "Aequitas", "prefix": "Aeq"},
                "acquired": "2026-02-26T17:20:41.785000Z",
                "location": {"start":[-955,-1415],"end":[-756,-1748]}
            }
        }"#;

        let parsed = parse_wynncraft_territory_payload(payload.as_bytes())
            .expect("payload with null guild should parse");

        let lion = parsed.get("Lion Lair").expect("lion lair should exist");
        assert_eq!(lion.guild.uuid, UNCLAIMED_GUILD_UUID);
        assert_eq!(lion.guild.name, UNCLAIMED_GUILD_NAME);
        assert_eq!(lion.guild.prefix, UNCLAIMED_GUILD_PREFIX);

        let ragni = parsed.get("Ragni").expect("ragni should exist");
        assert_eq!(ragni.guild.uuid, "abc");
        assert_eq!(ragni.guild.name, "Aequitas");
        assert_eq!(ragni.guild.prefix, "Aeq");
    }

    #[tokio::test]
    async fn skips_noop_tick_when_there_are_no_changes() {
        let state = AppState::new(Some(lazy_test_pool()));
        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 31;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(31, std::sync::atomic::Ordering::Relaxed);

        let mut rx = state.event_tx.subscribe();
        let unchanged_map = single_territory_map("g1", "GuildOne", "G1");

        process_polled_map_with(&state, unchanged_map, false, |_pool, _updates| {
            Box::pin(async { Err("persist should not be called on no-op ticks".to_string()) })
        })
        .await;

        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 31);
            assert_eq!(current.timestamp, "2026-01-01T00:00:00Z");
            assert_eq!(
                current
                    .territories
                    .get("Alpha")
                    .expect("territory should remain")
                    .guild
                    .uuid,
                "g1"
            );
        }
        assert_eq!(
            state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
            31
        );
    }

    #[tokio::test]
    async fn supplemental_only_tick_updates_snapshot_without_advancing_sequence() {
        let state = AppState::new(Some(lazy_test_pool()));
        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 31;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(31, std::sync::atomic::Ordering::Relaxed);

        let mut rx = state.event_tx.subscribe();
        let mut supplemented_map = single_territory_map("g1", "GuildOne", "G1");
        supplemented_map
            .get_mut("Alpha")
            .expect("territory should exist")
            .guild
            .color = Some((1, 2, 3));

        process_polled_map_with(&state, supplemented_map, true, |_pool, _updates| {
            Box::pin(async {
                Err("persist should not be called on supplemental-only ticks".to_string())
            })
        })
        .await;

        match rx.try_recv() {
            Ok(PreSerializedEvent::Snapshot { seq, .. }) => assert_eq!(seq, 31),
            other => panic!("expected supplemental tick to emit snapshot event, got {other:?}"),
        }
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 31);
            assert_eq!(current.timestamp, "2026-01-01T00:00:00Z");
            let alpha = current
                .territories
                .get("Alpha")
                .expect("territory should remain");
            assert_eq!(alpha.guild.uuid, "g1");
            assert_eq!(alpha.guild.color, Some((1, 2, 3)));

            let territories_json: serde_json::Value =
                serde_json::from_slice(current.territories_json.as_ref())
                    .expect("territories json should parse");
            assert_eq!(
                territories_json["Alpha"]["guild"]["color"],
                serde_json::json!([1, 2, 3])
            );
        }
        assert_eq!(
            state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
            31
        );
    }

    #[tokio::test]
    async fn ownership_change_keeps_cached_guild_colors_when_supplemental_is_not_dirty() {
        let state = AppState::new(Some(lazy_test_pool()));
        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 41;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(41, std::sync::atomic::Ordering::Relaxed);

        let mut new_map = single_territory_map("g2", "GuildTwo", "G2");
        let mut cached_colors = HashMap::new();
        cached_colors.insert("GuildTwo".to_string(), (1, 2, 3));

        merge_supplemental_data(
            &mut new_map,
            &HashMap::new(),
            &cached_colors,
            &crate::state::build_guild_color_lookup(&cached_colors),
            &HashMap::new(),
            Duration::from_secs(180),
        );
        assert_eq!(
            new_map
                .get("Alpha")
                .expect("territory should exist")
                .guild
                .color,
            Some((1, 2, 3))
        );

        process_polled_map_with(&state, new_map, false, |_pool, _updates| {
            Box::pin(async { Ok(()) })
        })
        .await;

        {
            let current = state.live_snapshot.read().await;
            let alpha = current
                .territories
                .get("Alpha")
                .expect("territory should update");
            assert_eq!(alpha.guild.uuid, "g2");
            assert_eq!(alpha.guild.color, Some((1, 2, 3)));

            let territories_json: serde_json::Value =
                serde_json::from_slice(current.territories_json.as_ref())
                    .expect("territories json should parse");
            assert_eq!(
                territories_json["Alpha"]["guild"]["color"],
                serde_json::json!([1, 2, 3])
            );
        }
    }

    #[test]
    fn merge_supplemental_data_matches_guild_colors_case_insensitively() {
        let mut new_map = single_territory_map("g2", "  AVICIA  ", "AVO");
        let mut cached_colors = HashMap::new();
        cached_colors.insert("Avicia".to_string(), (16, 16, 254));
        let normalized = crate::state::build_guild_color_lookup(&cached_colors);

        merge_supplemental_data(
            &mut new_map,
            &HashMap::new(),
            &cached_colors,
            &normalized,
            &HashMap::new(),
            Duration::from_secs(180),
        );

        assert_eq!(
            new_map
                .get("Alpha")
                .expect("territory should exist")
                .guild
                .color,
            Some((16, 16, 254))
        );
    }

    #[tokio::test]
    async fn waits_for_persist_before_advancing_live_and_broadcasting() {
        let state = AppState::new(Some(lazy_test_pool()));

        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 7;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(7, std::sync::atomic::Ordering::Relaxed);

        let mut rx = state.event_tx.subscribe();
        let new_map = single_territory_map("g2", "GuildTwo", "G2");
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let persist_started = Arc::new(AtomicBool::new(false));
        let persist_started_for_task = Arc::clone(&persist_started);

        let worker = tokio::spawn({
            let state = state.clone();
            async move {
                process_polled_map_with(&state, new_map, false, move |_pool, updates| {
                    Box::pin(async move {
                        assert_eq!(updates.len(), 1);
                        assert_eq!(updates[0].0, 8);
                        persist_started_for_task.store(true, AtomicOrdering::Relaxed);
                        let _ = release_rx.await;
                        Ok(())
                    })
                })
                .await;
            }
        });

        while !persist_started.load(AtomicOrdering::Relaxed) {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 7);
            assert_eq!(
                current
                    .territories
                    .get("Alpha")
                    .expect("territory should remain")
                    .guild
                    .uuid,
                "g1"
            );
        }
        assert_eq!(state.next_seq.load(std::sync::atomic::Ordering::Relaxed), 7);

        let _ = release_tx.send(());
        worker.await.expect("worker should complete");

        match rx.try_recv() {
            Ok(PreSerializedEvent::Update { seq, .. }) => assert_eq!(seq, 8),
            Ok(other) => panic!("expected update event, got {other:?}"),
            Err(e) => panic!("expected update event, got recv error: {e}"),
        }
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 8);
            assert_eq!(
                current
                    .territories
                    .get("Alpha")
                    .expect("territory should exist")
                    .guild
                    .uuid,
                "g2"
            );
        }
        assert_eq!(state.next_seq.load(std::sync::atomic::Ordering::Relaxed), 8);
        let observability = state.observability.snapshot();
        assert_eq!(observability.persisted_update_events_total, 1);
        assert_eq!(observability.dropped_update_events_total, 0);
        assert_eq!(observability.persist_failures_total, 0);
    }

    #[tokio::test]
    async fn continues_live_update_when_database_is_unavailable() {
        let state = AppState::new(None);

        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 11;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(11, std::sync::atomic::Ordering::Relaxed);

        let mut rx = state.event_tx.subscribe();
        let new_map = single_territory_map("g2", "GuildTwo", "G2");

        process_polled_map_with(&state, new_map, false, |_pool, _updates| {
            Box::pin(async { Ok(()) })
        })
        .await;

        match rx.try_recv() {
            Ok(PreSerializedEvent::Update { seq, .. }) => assert_eq!(seq, 12),
            Ok(other) => panic!("expected update event, got {other:?}"),
            Err(e) => panic!("expected update event, got recv error: {e}"),
        }
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 12);
            assert_eq!(
                current
                    .territories
                    .get("Alpha")
                    .expect("territory should update")
                    .guild
                    .uuid,
                "g2"
            );
        }
        assert_eq!(
            state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
            12
        );
        let observability = state.observability.snapshot();
        assert_eq!(observability.persisted_update_events_total, 0);
        assert_eq!(observability.dropped_update_events_total, 1);
        assert_eq!(observability.persist_failures_total, 0);
    }

    #[tokio::test]
    async fn continues_live_update_when_persist_fails() {
        let state = AppState::new(Some(lazy_test_pool()));

        {
            let mut current = state.live_snapshot.write().await;
            current.territories = single_territory_map("g1", "GuildOne", "G1");
            current.seq = 21;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(21, std::sync::atomic::Ordering::Relaxed);

        let mut rx = state.event_tx.subscribe();
        let new_map = single_territory_map("g2", "GuildTwo", "G2");

        process_polled_map_with(&state, new_map, false, |_pool, _updates| {
            Box::pin(async { Err("forced persist error".to_string()) })
        })
        .await;

        match rx.try_recv() {
            Ok(PreSerializedEvent::Update { seq, .. }) => assert_eq!(seq, 22),
            Ok(other) => panic!("expected update event, got {other:?}"),
            Err(e) => panic!("expected update event, got recv error: {e}"),
        }
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 22);
            assert_eq!(
                current
                    .territories
                    .get("Alpha")
                    .expect("territory should update")
                    .guild
                    .uuid,
                "g2"
            );
        }
        assert_eq!(
            state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
            22
        );
        let observability = state.observability.snapshot();
        assert_eq!(observability.persisted_update_events_total, 0);
        assert_eq!(observability.dropped_update_events_total, 1);
        assert_eq!(observability.persist_failures_total, 1);
    }

    #[tokio::test]
    async fn persists_updates_and_serves_history_endpoints_with_real_postgres() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping real-Postgres integration test: DATABASE_URL is not set");
            return;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("connect real postgres");
        let mut lock_conn = pool.acquire().await.expect("acquire lock connection");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(73_019_001_i64)
            .execute(&mut *lock_conn)
            .await
            .expect("acquire history test db lock");
        crate::db_migrations::run(&pool)
            .await
            .expect("run migrations");
        sqlx::query(
            "TRUNCATE TABLE territory_events, territory_snapshots, guild_color_cache RESTART IDENTITY",
        )
            .execute(&pool)
            .await
            .expect("truncate history tables");

        let state = AppState::new(Some(pool.clone()));
        {
            let mut initial = single_territory_map("g1", "GuildOne", "G1");
            initial
                .get_mut("Alpha")
                .expect("initial territory should exist")
                .guild
                .color = Some((11, 22, 33));
            let mut current = state.live_snapshot.write().await;
            current.territories = initial;
            current.seq = 0;
            current.timestamp = "2026-01-01T00:00:00Z".to_string();
        }
        state
            .next_seq
            .store(0, std::sync::atomic::Ordering::Relaxed);

        let mut updated = single_territory_map("g2", "GuildTwo", "G2");
        updated
            .get_mut("Alpha")
            .expect("updated territory should exist")
            .guild
            .color = Some((44, 55, 66));

        process_polled_map_with(&state, updated, false, |pool, updates| {
            Box::pin(async move { super::persist_updates(pool, updates).await })
        })
        .await;

        let db_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM territory_events")
            .fetch_one(&pool)
            .await
            .expect("count history rows");
        assert_eq!(db_count.0, 1);

        let app = history_test_app(state.clone());
        let (addr, server_handle) = spawn_test_server(app).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let bounds = client
            .get(format!("{base_url}/api/history/bounds"))
            .send()
            .await
            .expect("history bounds request")
            .error_for_status()
            .expect("history bounds status")
            .json::<HistoryBounds>()
            .await
            .expect("parse history bounds");
        assert_eq!(bounds.event_count, 1);
        assert_eq!(bounds.latest_seq, Some(1));

        let to = (Utc::now() + chrono::TimeDelta::minutes(1)).to_rfc3339();
        let events_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/events"))
                .expect("history events url");
            url.query_pairs_mut()
                .append_pair("from", "1970-01-01T00:00:00Z")
                .append_pair("to", &to)
                .append_pair("limit", "100");
            url
        };
        let events = client
            .get(events_url)
            .send()
            .await
            .expect("history events request")
            .error_for_status()
            .expect("history events status")
            .json::<HistoryEvents>()
            .await
            .expect("parse history events");
        assert_eq!(events.events.len(), 1);
        let event = &events.events[0];
        assert_eq!(event.stream_seq, 1);
        assert_eq!(event.territory, "Alpha");
        assert_eq!(event.guild_uuid, "g2");
        assert_eq!(event.guild_color, Some((44, 55, 66)));
        assert_eq!(event.prev_guild_name.as_deref(), Some("GuildOne"));
        assert_eq!(event.prev_guild_color, Some((11, 22, 33)));

        let at_url = {
            let mut url =
                reqwest::Url::parse(&format!("{base_url}/api/history/at")).expect("history at url");
            url.query_pairs_mut().append_pair("t", &to);
            url
        };
        let snapshot = client
            .get(at_url)
            .send()
            .await
            .expect("history at request")
            .error_for_status()
            .expect("history at status")
            .json::<HistorySnapshot>()
            .await
            .expect("parse history snapshot");
        let alpha = snapshot
            .ownership
            .get("Alpha")
            .expect("Alpha should exist in history snapshot");
        assert_eq!(alpha.guild_uuid, "g2");
        assert_eq!(alpha.guild_name, "GuildTwo");
        assert_eq!(alpha.guild_color, Some((44, 55, 66)));

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(73_019_001_i64)
            .execute(&mut *lock_conn)
            .await
            .expect("release history test db lock");

        server_handle.abort();
        let _ = server_handle.await;
    }
}
