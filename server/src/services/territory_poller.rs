use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use sequoia_shared::{
    DataProvenance, GuildRef, Resources, Territory, TerritoryChange, TerritoryMap,
    TerritoryRuntimeChange, TerritoryRuntimeData, VisibilityClass,
};
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
            if terr.resources.is_empty() && !info.resources.is_empty() {
                terr.resources = info.resources.clone();
            }
            if terr.connections.is_empty() && !info.connections.is_empty() {
                terr.connections = info.connections.clone();
            }
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
    // 1. Read lock: compute ownership/runtime/static diffs, then release.
    let (
        changes,
        runtime_updates,
        has_static_field_changes,
        has_removals,
        mut live_seq,
        mut live_timestamp,
    ) = {
        let current = state.live_snapshot.read().await;
        let changes = compute_diff(&current.territories, &new_map);
        let changed_territories = changes
            .iter()
            .map(|change| change.territory.clone())
            .collect::<HashSet<_>>();
        let runtime_updates =
            compute_runtime_updates(&current.territories, &new_map, &changed_territories);
        (
            changes,
            runtime_updates,
            has_static_field_changes(&current.territories, &new_map),
            has_removed_territories(&current.territories, &new_map),
            current.seq,
            current.timestamp.clone(),
        )
    };
    let emit_snapshot_event = has_removals || supplemental_changed || has_static_field_changes;

    if changes.is_empty() && runtime_updates.is_empty() && !emit_snapshot_event {
        return;
    }

    let mut reserved_count = match u64::try_from(changes.len()) {
        Ok(count) => count,
        Err(_) => {
            warn!("too many territory changes to reserve sequence range");
            return;
        }
    };
    if !runtime_updates.is_empty() {
        reserved_count = match reserved_count.checked_add(1) {
            Some(count) => count,
            None => {
                warn!("sequence counter overflow while reserving runtime update event");
                return;
            }
        };
    }
    if emit_snapshot_event {
        reserved_count = match reserved_count.checked_add(1) {
            Some(count) => count,
            None => {
                warn!("sequence counter overflow while reserving snapshot event");
                return;
            }
        };
    }
    let Some(mut seq_cursor) = reserve_next_seq_block(state, reserved_count) else {
        warn!("sequence counter overflow while reserving sequence range");
        return;
    };
    let mut outgoing = Vec::new();
    let mut sequenced_updates: SequencedUpdates = Vec::new();
    let timestamp = Utc::now().to_rfc3339();

    if !changes.is_empty() {
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

    if !runtime_updates.is_empty() {
        let Some(seq) = seq_cursor.checked_add(1) else {
            warn!("Sequence counter overflow while preparing runtime update event");
            return;
        };
        seq_cursor = seq;
        let update_json = match serialize_runtime_update_event(
            seq,
            &runtime_updates,
            &timestamp,
            "runtime update broadcast event",
        ) {
            Some(json) => json,
            None => return,
        };
        live_seq = seq;
        live_timestamp = timestamp.clone();
        outgoing.push(PreSerializedEvent::RuntimeUpdate {
            seq,
            json: update_json,
        });
    }

    if emit_snapshot_event {
        let Some(seq) = seq_cursor.checked_add(1) else {
            warn!("Sequence counter overflow while preparing snapshot event");
            return;
        };
        seq_cursor = seq;
        if has_removals {
            info!("territory set changed (removals detected), broadcasting snapshot");
        } else if has_static_field_changes {
            info!("territory metadata changed, broadcasting snapshot");
        }
        live_seq = seq;
        live_timestamp = timestamp.clone();
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
    RuntimeUpdate {
        seq: u64,
        updates: &'a [TerritoryRuntimeChange],
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

fn serialize_runtime_update_event(
    seq: u64,
    updates: &[TerritoryRuntimeChange],
    timestamp: &str,
    context: &str,
) -> Option<Arc<Bytes>> {
    match serde_json::to_vec(&SerializedUpdateEvent::RuntimeUpdate {
        seq,
        updates,
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
    #[serde(default)]
    hq: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(untagged)]
enum RawResourcePayload {
    List(Vec<RawTerritoryResource>),
    Legacy(Resources),
    #[default]
    Empty,
}

#[derive(Debug, serde::Deserialize)]
struct RawTerritoryResource {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "generation")]
    generation: i64,
    #[serde(default, rename = "baseGeneration")]
    base_generation: i64,
    #[serde(default)]
    stored: i64,
    #[serde(default)]
    limit: i64,
}

#[derive(serde::Deserialize)]
struct RawTerritory {
    #[serde(default)]
    guild: Option<RawGuildRef>,
    acquired: chrono::DateTime<chrono::Utc>,
    location: sequoia_shared::Region,
    #[serde(default)]
    resources: RawResourcePayload,
    #[serde(default)]
    links: Vec<String>,
    #[serde(default)]
    connections: Vec<String>,
    #[serde(default)]
    hq: Option<bool>,
    #[serde(default)]
    treasury: Option<String>,
    #[serde(default)]
    defences: Option<String>,
}

impl RawTerritory {
    fn into_territory(self, observed_at: DateTime<Utc>) -> Territory {
        let value = self;
        let guild = value.guild.unwrap_or(RawGuildRef {
            uuid: None,
            name: None,
            prefix: None,
            hq: None,
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
        let (resources, held_resources, production_rates, storage_capacity) =
            split_resource_payload(value.resources);
        let connections = if value.links.is_empty() {
            value.connections
        } else {
            value.links
        };
        let runtime = build_api_runtime(ApiRuntimeParts {
            hq: value.hq,
            headquarters_territory: guild.hq,
            treasury: value.treasury,
            defences: value.defences,
            held_resources,
            production_rates,
            storage_capacity,
            observed_at,
        });

        Territory {
            guild: GuildRef {
                uuid: guild_uuid,
                name: guild_name,
                prefix: guild_prefix,
                color: None,
            },
            acquired: value.acquired,
            location: value.location,
            resources,
            connections,
            runtime,
        }
    }
}

fn parse_wynncraft_territory_payload(bytes: &[u8]) -> Result<TerritoryMap, serde_json::Error> {
    let raw_map: HashMap<String, RawTerritory> = serde_json::from_slice(bytes)?;
    let observed_at = Utc::now();
    Ok(raw_map
        .into_iter()
        .map(|(territory, raw)| (territory, raw.into_territory(observed_at)))
        .collect())
}

fn split_resource_payload(
    payload: RawResourcePayload,
) -> (
    Resources,
    Option<Resources>,
    Option<Resources>,
    Option<Resources>,
) {
    match payload {
        RawResourcePayload::Legacy(resources) => (resources, None, None, None),
        RawResourcePayload::List(entries) => {
            let mut base = Resources::default();
            let mut held = Resources::default();
            let mut production = Resources::default();
            let mut capacity = Resources::default();
            for entry in entries {
                // v3.7.2 currently returns baseGeneration for every resource slot.
                // generation > 0 is the reliable signal that the territory produces it.
                if entry.generation > 0 {
                    set_resource_value(&mut base, &entry.kind, entry.base_generation);
                }
                set_resource_value(&mut held, &entry.kind, entry.stored);
                set_resource_value(&mut production, &entry.kind, entry.generation);
                set_resource_value(&mut capacity, &entry.kind, entry.limit);
            }
            (
                base,
                (!held.is_empty()).then_some(held),
                (!production.is_empty()).then_some(production),
                (!capacity.is_empty()).then_some(capacity),
            )
        }
        RawResourcePayload::Empty => (Resources::default(), None, None, None),
    }
}

fn set_resource_value(resources: &mut Resources, kind: &str, value: i64) {
    let value = i32::try_from(value.max(0)).unwrap_or(i32::MAX);
    match kind.trim().to_ascii_uppercase().as_str() {
        "EMERALD" | "EMERALDS" => resources.emeralds = value,
        "ORE" => resources.ore = value,
        "CROP" | "CROPS" => resources.crops = value,
        "FISH" => resources.fish = value,
        "WOOD" => resources.wood = value,
        _ => {}
    }
}

fn clean_api_tier(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().replace('_', " "))
        .filter(|value| !value.is_empty())
}

struct ApiRuntimeParts {
    hq: Option<bool>,
    headquarters_territory: Option<String>,
    treasury: Option<String>,
    defences: Option<String>,
    held_resources: Option<Resources>,
    production_rates: Option<Resources>,
    storage_capacity: Option<Resources>,
    observed_at: DateTime<Utc>,
}

fn build_api_runtime(parts: ApiRuntimeParts) -> Option<TerritoryRuntimeData> {
    let headquarters_territory = parts
        .headquarters_territory
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let treasury = clean_api_tier(parts.treasury);
    let defense_tier = clean_api_tier(parts.defences);
    if parts.hq.is_none()
        && headquarters_territory.is_none()
        && treasury.is_none()
        && defense_tier.is_none()
        && parts.held_resources.is_none()
        && parts.production_rates.is_none()
        && parts.storage_capacity.is_none()
    {
        return None;
    }

    Some(TerritoryRuntimeData {
        headquarters: parts.hq,
        headquarters_territory,
        held_resources: parts.held_resources,
        production_rates: parts.production_rates,
        storage_capacity: parts.storage_capacity,
        treasury,
        defense_tier,
        contested: None,
        active_war: None,
        extra_scrapes: None,
        provenance: Some(DataProvenance {
            source: "wynncraft_api".to_string(),
            visibility: VisibilityClass::Public,
            confidence: 1.0,
            reporter_count: 1,
            observed_at: parts.observed_at.to_rfc3339(),
            menu_season_id: None,
            menu_captured_territories: None,
            menu_sr_per_hour: None,
            menu_observed_at: None,
        }),
    })
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

fn compute_runtime_updates(
    old: &TerritoryMap,
    new: &TerritoryMap,
    ownership_changes: &HashSet<String>,
) -> Vec<TerritoryRuntimeChange> {
    new.iter()
        .filter(|(name, _)| !ownership_changes.contains(*name))
        .filter_map(|(name, new_territory)| {
            let old_runtime = old
                .get(name)
                .and_then(|territory| territory.runtime.as_ref());
            if runtime_payload_eq(old_runtime, new_territory.runtime.as_ref()) {
                return None;
            }
            Some(TerritoryRuntimeChange {
                territory: name.clone(),
                runtime: new_territory.runtime.clone(),
            })
        })
        .collect()
}

fn runtime_payload_eq(
    left: Option<&TerritoryRuntimeData>,
    right: Option<&TerritoryRuntimeData>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.headquarters == right.headquarters
                && left.headquarters_territory == right.headquarters_territory
                && left.held_resources == right.held_resources
                && left.production_rates == right.production_rates
                && left.storage_capacity == right.storage_capacity
                && left.treasury == right.treasury
                && left.defense_tier == right.defense_tier
                && left.contested == right.contested
                && left.active_war == right.active_war
                && left.extra_scrapes == right.extra_scrapes
                && runtime_provenance_eq(left.provenance.as_ref(), right.provenance.as_ref())
        }
        _ => false,
    }
}

fn runtime_provenance_eq(left: Option<&DataProvenance>, right: Option<&DataProvenance>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.source == right.source
                && left.visibility == right.visibility
                && left.confidence == right.confidence
                && left.reporter_count == right.reporter_count
                && left.menu_season_id == right.menu_season_id
                && left.menu_captured_territories == right.menu_captured_territories
                && left.menu_sr_per_hour == right.menu_sr_per_hour
                && left.menu_observed_at == right.menu_observed_at
        }
        _ => false,
    }
}

fn has_static_field_changes(old: &TerritoryMap, new: &TerritoryMap) -> bool {
    new.iter().any(|(name, new_territory)| {
        old.get(name).is_some_and(|old_territory| {
            old_territory.location != new_territory.location
                || old_territory.resources != new_territory.resources
                || old_territory.connections != new_territory.connections
                || old_territory.guild.color != new_territory.guild.color
        })
    })
}

fn has_removed_territories(old: &TerritoryMap, new: &TerritoryMap) -> bool {
    old.keys().any(|name| !new.contains_key(name))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::time::Duration;

    use super::{
        UNCLAIMED_GUILD_NAME, UNCLAIMED_GUILD_PREFIX, UNCLAIMED_GUILD_UUID, compute_diff,
        compute_runtime_updates, has_removed_territories, merge_supplemental_data,
        parse_wynncraft_territory_payload, process_polled_map_with,
    };
    use axum::Router;
    use chrono::{DateTime, Utc};
    use sequoia_shared::history::{HistoryBounds, HistoryEvents, HistorySnapshot};
    use sequoia_shared::{
        DataProvenance, GuildRef, Region, Territory, TerritoryMap, TerritoryRuntimeData,
    };
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
    fn runtime_changes_are_emitted_separately_from_ownership_changes() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));

        let mut new = old.clone();
        new.get_mut("Alpha").expect("alpha should exist").runtime = Some(TerritoryRuntimeData {
            contested: Some(true),
            ..TerritoryRuntimeData::default()
        });

        let diff = compute_diff(&old, &new);
        assert!(diff.is_empty());

        let runtime_updates = compute_runtime_updates(&old, &new, &HashSet::new());
        assert_eq!(runtime_updates.len(), 1);
        assert_eq!(runtime_updates[0].territory, "Alpha");
        assert_eq!(
            runtime_updates[0]
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.contested),
            Some(true)
        );
    }

    #[test]
    fn runtime_updates_ignore_volatile_provenance_observed_at() {
        let mut old = TerritoryMap::new();
        old.insert("Alpha".to_string(), territory("g1", "GuildOne", "G1"));
        old.get_mut("Alpha").expect("alpha should exist").runtime = Some(TerritoryRuntimeData {
            treasury: Some("low".to_string()),
            provenance: Some(DataProvenance {
                source: "wynncraft_api".to_string(),
                observed_at: "2026-05-25T12:00:00Z".to_string(),
                confidence: 1.0,
                ..DataProvenance::default()
            }),
            ..TerritoryRuntimeData::default()
        });

        let mut new = old.clone();
        new.get_mut("Alpha")
            .and_then(|territory| territory.runtime.as_mut())
            .and_then(|runtime| runtime.provenance.as_mut())
            .expect("runtime provenance should exist")
            .observed_at = "2026-05-25T12:00:10Z".to_string();

        let runtime_updates = compute_runtime_updates(&old, &new, &HashSet::new());
        assert!(runtime_updates.is_empty());

        new.get_mut("Alpha")
            .and_then(|territory| territory.runtime.as_mut())
            .expect("runtime should exist")
            .treasury = Some("high".to_string());

        let runtime_updates = compute_runtime_updates(&old, &new, &HashSet::new());
        assert_eq!(runtime_updates.len(), 1);
        assert_eq!(runtime_updates[0].territory, "Alpha");
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

    #[test]
    fn parse_wynncraft_payload_uses_direct_api_runtime_fields() {
        let payload = r#"{
            "Forts in Fall": {
                "guild": {
                    "uuid": "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b",
                    "name": "Sequoia",
                    "prefix": "SEQ",
                    "hq": "Forts in Fall"
                },
                "acquired": "2026-05-24T04:06:09.888000Z",
                "location": {"start":[-2039,-1500],"end":[-1780,-1134]},
                "hq": true,
                "resources": [
                    {"type":"EMERALD","generation":86400,"baseGeneration":9000,"stored":381631,"limit":400000},
                    {"type":"WOOD","generation":43200,"baseGeneration":3600,"stored":119989,"limit":120000},
                    {"type":"CROP","generation":0,"baseGeneration":3600,"stored":72834,"limit":120000}
                ],
                "links": ["Fort Torann", "Royal Dam"],
                "treasury": "MEDIUM",
                "defences": "VERY_HIGH"
            }
        }"#;

        let parsed = parse_wynncraft_territory_payload(payload.as_bytes())
            .expect("new territory payload should parse");
        let territory = parsed
            .get("Forts in Fall")
            .expect("territory should be present");

        assert_eq!(territory.resources.emeralds, 9000);
        assert_eq!(territory.resources.wood, 3600);
        assert_eq!(territory.resources.crops, 0);
        assert_eq!(territory.connections, ["Fort Torann", "Royal Dam"]);

        let runtime = territory
            .runtime
            .as_ref()
            .expect("runtime should be present");
        assert_eq!(runtime.headquarters, Some(true));
        assert_eq!(
            runtime.headquarters_territory.as_deref(),
            Some("Forts in Fall")
        );
        assert_eq!(runtime.treasury.as_deref(), Some("MEDIUM"));
        assert_eq!(runtime.defense_tier.as_deref(), Some("VERY HIGH"));
        assert_eq!(
            runtime
                .held_resources
                .as_ref()
                .map(|resources| resources.emeralds),
            Some(381631)
        );
        assert_eq!(
            runtime
                .production_rates
                .as_ref()
                .map(|resources| resources.wood),
            Some(43200)
        );
        assert_eq!(
            runtime
                .storage_capacity
                .as_ref()
                .map(|resources| resources.crops),
            Some(120000)
        );
        assert_eq!(
            runtime.provenance.as_ref().map(|p| p.source.as_str()),
            Some("wynncraft_api")
        );
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
    async fn supplemental_only_tick_updates_snapshot_with_fresh_sequence() {
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
            Ok(PreSerializedEvent::Snapshot { seq, .. }) => assert_eq!(seq, 32),
            other => panic!("expected supplemental tick to emit snapshot event, got {other:?}"),
        }
        {
            let current = state.live_snapshot.read().await;
            assert_eq!(current.seq, 32);
            assert_ne!(current.timestamp, "2026-01-01T00:00:00Z");
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
            32
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
