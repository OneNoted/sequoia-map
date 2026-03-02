use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sequoia_shared::{
    BASE_HOURLY_SR, CanonicalTerritoryBatch, CanonicalTerritoryUpdate, SeasonScalarCurrent,
    SeasonScalarSample, TerritoryChange, TerritoryEvent, TerritoryMap, TerritoryRuntimeChange,
    weighted_units,
};
use tracing::{info, warn};

use crate::state::{
    AppState, IngestTerritoryOverride, PreSerializedEvent, build_guild_color_lookup,
    lookup_guild_color, normalize_guild_color_key,
};

const INTERNAL_INGEST_HEADER: &str = "x-internal-ingest-token";
const MAX_OVERRIDE_OBSERVED_AT_FUTURE_SKEW_SECS: i64 = 30;

pub async fn ingest_territory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(batch): Json<CanonicalTerritoryBatch>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    ensure_internal_ingest_auth(&state, &headers)?;
    if batch.updates.len() > state.max_ingest_updates_per_request {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    state
        .observability
        .record_ingest_reports(batch.updates.len() as u64);

    let mut rejected = 0_u64;
    let mut applied = 0_u64;
    let mut degraded = 0_u64;

    let mut outgoing: Vec<PreSerializedEvent> = Vec::new();
    let mut runtime_updates: Vec<TerritoryRuntimeChange> = Vec::new();
    let mut ownership_updates: Vec<TerritoryChange> = Vec::new();
    let mut override_updates: Vec<(String, IngestTerritoryOverride)> = Vec::new();
    let mut snapshot_event: Option<PreSerializedEvent> = None;
    let timestamp = Utc::now().to_rfc3339();
    let mut latest_seq: u64;
    let mut accepted_payloads: Vec<CanonicalTerritoryUpdate> = Vec::new();
    let cached_colors = state.guild_colors.read().await.clone();
    let cached_colors_normalized = build_guild_color_lookup(&cached_colors);
    let mut needs_snapshot_event = false;

    {
        let mut snapshot = state.live_snapshot.write().await;
        latest_seq = snapshot.seq;

        for mut update in batch.updates {
            let Some(territory) = snapshot.territories.get_mut(&update.territory) else {
                rejected += 1;
                continue;
            };

            let mut changed = false;
            let mut ownership_changed = false;
            let mut non_event_field_changed = false;
            let previous_guild = territory.guild.clone();
            let now = Utc::now();
            let mut observed_at = now;
            let mut confidence = 1.0_f32;

            if let Some(mut guild) = update.guild.clone() {
                if guild.color.is_none() {
                    guild.color =
                        lookup_guild_color(&cached_colors, &cached_colors_normalized, &guild.name);
                    if guild.color.is_none()
                        && normalize_guild_color_key(&guild.name)
                            == normalize_guild_color_key(&territory.guild.name)
                    {
                        guild.color = territory.guild.color;
                    }
                }
                if territory.guild != guild {
                    territory.guild = guild.clone();
                    update.guild = Some(guild);
                    changed = true;
                    ownership_changed = true;
                }
            }

            let parsed_acquired = parse_optional_rfc3339(update.acquired.as_deref());
            if let Some(acquired) = parsed_acquired
                && territory.acquired != acquired
            {
                territory.acquired = acquired;
                changed = true;
                ownership_changed = true;
            }

            if let Some(location) = update.location.clone()
                && territory.location != location
            {
                territory.location = location;
                changed = true;
                non_event_field_changed = true;
            }

            if let Some(resources) = update.resources.clone()
                && territory.resources != resources
            {
                territory.resources = resources;
                changed = true;
                non_event_field_changed = true;
            }

            if let Some(connections) = update.connections.clone()
                && territory.connections != connections
            {
                territory.connections = connections;
                changed = true;
                non_event_field_changed = true;
            }

            if let Some(runtime) = update.runtime.clone() {
                if let Some(provenance) = runtime.provenance.as_ref() {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(&provenance.observed_at) {
                        observed_at = dt.with_timezone(&Utc);
                    }
                    observed_at = sanitize_override_observed_at(observed_at, now);
                    confidence = provenance.confidence.clamp(0.0, 1.0);
                    if confidence < 0.66 {
                        degraded += 1;
                    }
                }
                if territory.runtime.as_ref() != Some(&runtime) {
                    territory.runtime = Some(runtime.clone());
                    runtime_updates.push(TerritoryRuntimeChange {
                        territory: update.territory.clone(),
                        runtime: Some(runtime),
                    });
                    changed = true;
                }
            }

            if !changed {
                rejected += 1;
                continue;
            }

            if non_event_field_changed && !ownership_changed {
                // `update`/`runtime_update` events do not carry all non-ownership fields in this path.
                // Emit a sequenced snapshot event so clients always converge after accepted corrections.
                needs_snapshot_event = true;
            }

            if ownership_changed {
                ownership_updates.push(TerritoryChange {
                    territory: update.territory.clone(),
                    guild: territory.guild.clone(),
                    previous_guild: Some(previous_guild),
                    acquired: territory.acquired.to_rfc3339(),
                    location: territory.location.clone(),
                    resources: territory.resources.clone(),
                    connections: territory.connections.clone(),
                    runtime: territory.runtime.clone(),
                });
            }

            if update.guild.is_some() || update.acquired.is_some() || update.runtime.is_some() {
                override_updates.push((
                    update.territory.clone(),
                    IngestTerritoryOverride {
                        guild: update.guild.clone(),
                        acquired: parsed_acquired,
                        runtime: update.runtime.clone(),
                        observed_at,
                        confidence,
                    },
                ));
            }

            accepted_payloads.push(CanonicalTerritoryUpdate {
                territory: update.territory,
                guild: Some(territory.guild.clone()),
                acquired: Some(territory.acquired.to_rfc3339()),
                location: Some(territory.location.clone()),
                resources: Some(territory.resources.clone()),
                connections: Some(territory.connections.clone()),
                runtime: update.runtime.clone(),
                idempotency_key: update.idempotency_key,
            });

            applied += 1;
        }

        let event_count = u64::from(!ownership_updates.is_empty())
            + u64::from(!runtime_updates.is_empty())
            + u64::from(needs_snapshot_event);
        let mut reserved_seq = if event_count > 0 {
            reserve_next_seq_block(&state, event_count)?
        } else {
            latest_seq
        };

        if !ownership_updates.is_empty() {
            reserved_seq = next_seq(reserved_seq)?;
            latest_seq = reserved_seq;
            let payload = TerritoryEvent::Update {
                seq: latest_seq,
                changes: ownership_updates.clone(),
                timestamp: timestamp.clone(),
            };
            let json = serialize_event(payload)?;
            outgoing.push(PreSerializedEvent::Update {
                seq: latest_seq,
                json,
            });
        }

        if !runtime_updates.is_empty() {
            reserved_seq = next_seq(reserved_seq)?;
            latest_seq = reserved_seq;
            let payload = TerritoryEvent::RuntimeUpdate {
                seq: latest_seq,
                updates: runtime_updates.clone(),
                timestamp: timestamp.clone(),
            };
            let json = serialize_event(payload)?;
            outgoing.push(PreSerializedEvent::RuntimeUpdate {
                seq: latest_seq,
                json,
            });
        }

        if needs_snapshot_event {
            reserved_seq = next_seq(reserved_seq)?;
            latest_seq = reserved_seq;
        }

        if latest_seq != snapshot.seq {
            let (snapshot_json, territories_json, live_state_json, ownership_json) =
                serialize_all_formats(latest_seq, &timestamp, &snapshot.territories)?;

            if needs_snapshot_event {
                snapshot_event = Some(PreSerializedEvent::Snapshot {
                    seq: latest_seq,
                    json: Arc::clone(&snapshot_json),
                });
            }

            snapshot.seq = latest_seq;
            snapshot.timestamp = timestamp.clone();
            snapshot.snapshot_json = snapshot_json;
            snapshot.territories_json = territories_json;
            snapshot.live_state_json = live_state_json;
            snapshot.ownership_json = ownership_json;
        }
    }

    if !override_updates.is_empty() {
        let mut overrides = state.ingest_overrides.write().await;
        for (territory, override_info) in override_updates {
            if should_replace_ingest_override(overrides.get(&territory), &override_info) {
                overrides.insert(territory, override_info);
            }
        }
    }

    if latest_seq > state.next_seq.load(Ordering::Relaxed) {
        state.next_seq.store(latest_seq, Ordering::Relaxed);
    }

    if let Some(pool) = state.db.as_ref()
        && let Err(e) = persist_canonical_territory_updates(pool, &accepted_payloads).await
    {
        warn!("failed to persist canonical territory updates: {e}");
    }
    if let Err(e) = persist_authoritative_scalar_sample(&state, &accepted_payloads).await {
        warn!("failed to persist authoritative scalar sample: {e}");
    }

    if let Some(event) = snapshot_event {
        outgoing.push(event);
    }

    for event in outgoing {
        let _ = state.event_tx.send(event);
    }

    if rejected > 0 {
        state.observability.record_ingest_reports_rejected(rejected);
    }
    if applied > 0 {
        state.observability.record_ingest_reports_applied(applied);
    }
    if degraded > 0 {
        state.observability.record_ingest_reports_degraded(degraded);
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "applied": applied,
        "rejected": rejected,
        "degraded": degraded,
        "latest_seq": latest_seq,
    })))
}

pub async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    ensure_internal_ingest_auth(&state, &headers)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "timestamp": Utc::now().to_rfc3339(),
        "seq": state.next_seq.load(Ordering::Relaxed),
    })))
}

pub async fn get_live_wars() -> Response {
    let mut response = Json(Vec::<serde_json::Value>::new()).into_response();
    response
        .headers_mut()
        .insert("deprecation", HeaderValue::from_static("true"));
    response.headers_mut().insert(
        "warning",
        HeaderValue::from_static("299 - \"deprecated endpoint; always returns empty array\""),
    );
    response
}

fn ensure_internal_ingest_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = state.internal_ingest_token.as_deref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let provided = headers
        .get(INTERNAL_INGEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(provided, expected) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        let l = *left.get(idx).unwrap_or(&0);
        let r = *right.get(idx).unwrap_or(&0);
        diff |= usize::from(l ^ r);
    }
    diff == 0
}

fn parse_optional_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn sanitize_override_observed_at(observed_at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let max_allowed = now + chrono::Duration::seconds(MAX_OVERRIDE_OBSERVED_AT_FUTURE_SKEW_SECS);
    if observed_at > max_allowed {
        now
    } else {
        observed_at
    }
}

fn should_replace_ingest_override(
    existing: Option<&IngestTerritoryOverride>,
    incoming: &IngestTerritoryOverride,
) -> bool {
    let Some(existing) = existing else {
        return true;
    };

    if incoming.observed_at > existing.observed_at {
        return true;
    }
    if incoming.observed_at < existing.observed_at {
        return false;
    }

    incoming.confidence >= existing.confidence
}

fn next_seq(current: u64) -> Result<u64, StatusCode> {
    current
        .checked_add(1)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn reserve_next_seq_block(state: &AppState, count: u64) -> Result<u64, StatusCode> {
    if count == 0 {
        return Ok(state
            .next_seq_reserved
            .load(Ordering::Relaxed)
            .max(state.next_seq.load(Ordering::Relaxed)));
    }

    loop {
        let reserved = state.next_seq_reserved.load(Ordering::Relaxed);
        let committed = state.next_seq.load(Ordering::Relaxed);
        let base = reserved.max(committed);
        let next = base
            .checked_add(count)
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        match state.next_seq_reserved.compare_exchange_weak(
            reserved,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(base),
            Err(_) => continue,
        }
    }
}

fn serialize_event(event: TerritoryEvent) -> Result<Arc<Bytes>, StatusCode> {
    serde_json::to_vec(&event)
        .map(|json| Arc::new(Bytes::from(json)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

type SerializedSnapshotPayloads = (Arc<Bytes>, Arc<Bytes>, Arc<Bytes>, Arc<Bytes>);

fn serialize_all_formats(
    seq: u64,
    timestamp: &str,
    territories: &TerritoryMap,
) -> Result<SerializedSnapshotPayloads, StatusCode> {
    #[derive(serde::Serialize)]
    struct OwnershipEntryRef<'a> {
        guild_uuid: &'a str,
        guild_name: &'a str,
        guild_prefix: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        guild_color: Option<(u8, u8, u8)>,
        acquired_at: &'a chrono::DateTime<chrono::Utc>,
    }

    let territories_vec =
        serde_json::to_vec(territories).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let ownership_vec =
        serde_json::to_vec(&ownership_entries).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let timestamp_json =
        serde_json::to_string(timestamp).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

    Ok((
        Arc::new(Bytes::from(snapshot_buf)),
        territories_json,
        Arc::new(Bytes::from(live_state_buf)),
        Arc::new(Bytes::from(ownership_vec)),
    ))
}

async fn persist_canonical_territory_updates(
    pool: &sqlx::PgPool,
    updates: &[CanonicalTerritoryUpdate],
) -> Result<(), String> {
    for update in updates {
        let payload = serde_json::to_value(update)
            .map_err(|e| format!("serialize canonical territory payload: {e}"))?;
        let provenance = update
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.provenance.as_ref());
        let confidence = provenance.map(|value| value.confidence).unwrap_or(1.0);
        let source = provenance
            .map(|value| value.source.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let visibility = provenance
            .map(|value| match value.visibility {
                sequoia_shared::VisibilityClass::Public => "public",
                sequoia_shared::VisibilityClass::GuildOptIn => "guild_opt_in",
            })
            .unwrap_or("public");
        let reporter_count = provenance
            .map(|value| i32::from(value.reporter_count))
            .unwrap_or(0);
        let observed_at = provenance
            .and_then(|value| DateTime::parse_from_rfc3339(&value.observed_at).ok())
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        sqlx::query(
            "INSERT INTO canonical_territory_updates \
             (territory, observed_at, confidence, visibility, source, reporter_count, idempotency_key, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb) \
             ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING",
        )
        .bind(&update.territory)
        .bind(observed_at)
        .bind(confidence)
        .bind(visibility)
        .bind(source)
        .bind(reporter_count)
        .bind(update.idempotency_key.as_deref())
        .bind(payload)
        .execute(pool)
        .await
        .map_err(|e| format!("insert canonical_territory_updates row: {e}"))?;
    }
    Ok(())
}

async fn persist_authoritative_scalar_sample(
    state: &AppState,
    updates: &[CanonicalTerritoryUpdate],
) -> Result<(), String> {
    const MAX_REASONABLE_SCALAR: f64 = 20.0;
    const DUPLICATE_EPSILON: f64 = 0.0005;
    const AUTHORITATIVE_CONFIDENCE: f64 = 0.99;

    let Some(pool) = state.db.as_ref() else {
        return Ok(());
    };

    let mut candidate: Option<(DateTime<Utc>, i32, u16, i32)> = None;
    for update in updates {
        let Some(provenance) = update
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.provenance.as_ref())
        else {
            continue;
        };

        let Some(season_id) = provenance.menu_season_id else {
            continue;
        };
        let Some(captured_territories) = provenance.menu_captured_territories else {
            continue;
        };
        let Some(sr_per_hour) = provenance.menu_sr_per_hour else {
            continue;
        };
        if season_id <= 0 || captured_territories == 0 || sr_per_hour <= 0 {
            continue;
        }

        let observed_at = parse_optional_rfc3339(provenance.menu_observed_at.as_deref())
            .or_else(|| parse_optional_rfc3339(Some(provenance.observed_at.as_str())))
            .unwrap_or_else(Utc::now);

        if candidate
            .as_ref()
            .map(|(current, _, _, _)| observed_at > *current)
            .unwrap_or(true)
        {
            candidate = Some((observed_at, season_id, captured_territories, sr_per_hour));
        }
    }

    let Some((sampled_at, season_id, captured_territories, sr_per_hour)) = candidate else {
        return Ok(());
    };

    let weighted = weighted_units(captured_territories as usize);
    if weighted <= 0.0 {
        return Ok(());
    }
    let scalar_weighted = sr_per_hour as f64 / (BASE_HOURLY_SR * weighted);
    let scalar_raw = sr_per_hour as f64 / (BASE_HOURLY_SR * captured_territories as f64);
    if !scalar_weighted.is_finite()
        || !scalar_raw.is_finite()
        || scalar_weighted <= 0.0
        || scalar_raw <= 0.0
        || scalar_weighted > MAX_REASONABLE_SCALAR
        || scalar_raw > MAX_REASONABLE_SCALAR
    {
        return Ok(());
    }

    let should_insert = {
        let latest = state.latest_scalar_sample.read().await;
        match latest.as_ref() {
            Some((sample, _)) => !is_duplicate_scalar_sample(
                sample,
                season_id,
                scalar_weighted,
                scalar_raw,
                DUPLICATE_EPSILON,
            ),
            _ => true,
        }
    };
    if !should_insert {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO season_scalar_samples \
         (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(sampled_at)
    .bind(season_id)
    .bind(scalar_weighted)
    .bind(scalar_raw)
    .bind(AUTHORITATIVE_CONFIDENCE)
    .bind(1_i32)
    .execute(pool)
    .await
    .map_err(|e| format!("insert authoritative season scalar sample: {e}"))?;

    let sample = SeasonScalarSample {
        sampled_at: sampled_at.to_rfc3339(),
        season_id,
        scalar_weighted,
        scalar_raw,
        confidence: AUTHORITATIVE_CONFIDENCE,
        sample_count: 1,
    };
    if let Ok(json) = serde_json::to_vec(&SeasonScalarCurrent {
        sample: Some(sample.clone()),
    }) {
        let mut latest = state.latest_scalar_sample.write().await;
        *latest = Some((sample, Arc::new(Bytes::from(json))));
    }

    info!(
        season_id,
        captured_territories,
        sr_per_hour,
        scalar_weighted = format_args!("{scalar_weighted:.4}"),
        scalar_raw = format_args!("{scalar_raw:.4}"),
        "persisted authoritative season scalar sample from guild menu tooltip"
    );

    Ok(())
}

fn is_duplicate_scalar_sample(
    sample: &SeasonScalarSample,
    season_id: i32,
    scalar_weighted: f64,
    scalar_raw: f64,
    epsilon: f64,
) -> bool {
    sample.season_id == season_id
        && (sample.scalar_weighted - scalar_weighted).abs() < epsilon
        && (sample.scalar_raw - scalar_raw).abs() < epsilon
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use chrono::{DateTime, Duration, Utc};
    use sequoia_shared::{
        CanonicalTerritoryBatch, CanonicalTerritoryUpdate, DataProvenance, GuildRef, LiveState,
        Region, SeasonScalarSample, Territory, TerritoryRuntimeData, VisibilityClass,
    };

    use crate::routes::ingest::{
        constant_time_eq, ingest_territory, is_duplicate_scalar_sample,
        sanitize_override_observed_at, should_replace_ingest_override,
    };
    use crate::state::{AppState, IngestTerritoryOverride, PreSerializedEvent};

    #[tokio::test]
    async fn ingest_ownership_change_rehydrates_cached_guild_color() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("test-token".to_string());
        state
            .guild_colors
            .write()
            .await
            .insert("Paladins United".to_string(), (199, 179, 240));

        let initial_acquired = Utc::now();
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.territories.insert(
                "Molten Reach".to_string(),
                Territory {
                    guild: GuildRef {
                        uuid: "old-uuid".to_string(),
                        name: "Aequitas".to_string(),
                        prefix: "Aeq".to_string(),
                        color: Some((255, 215, 0)),
                    },
                    acquired: initial_acquired,
                    location: Region {
                        start: [0, 0],
                        end: [1, 1],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                    runtime: None,
                },
            );
        }

        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: vec![CanonicalTerritoryUpdate {
                territory: "Molten Reach".to_string(),
                guild: Some(GuildRef {
                    uuid: "new-uuid".to_string(),
                    name: "Paladins United".to_string(),
                    prefix: "PUN".to_string(),
                    color: None,
                }),
                acquired: Some((initial_acquired + Duration::seconds(1)).to_rfc3339()),
                location: None,
                resources: None,
                connections: None,
                runtime: None,
                idempotency_key: None,
            }],
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("test-token"),
        );

        let response = ingest_territory(State(state.clone()), headers, Json(batch))
            .await
            .expect("ingest should accept valid internal token");
        let body = response.0;
        assert_eq!(body["applied"], 1);
        assert_eq!(body["rejected"], 0);

        let snapshot = state.live_snapshot.read().await;
        let updated = snapshot
            .territories
            .get("Molten Reach")
            .expect("territory should remain present");
        assert_eq!(updated.guild.name, "Paladins United");
        assert_eq!(updated.guild.prefix, "PUN");
        assert_eq!(updated.guild.color, Some((199, 179, 240)));
    }

    #[tokio::test]
    async fn ingest_ownership_change_rehydrates_cached_guild_color_normalized_name() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("test-token".to_string());
        state
            .guild_colors
            .write()
            .await
            .insert("Avicia".to_string(), (16, 16, 254));

        let initial_acquired = Utc::now();
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.territories.insert(
                "Cinfras Outskirts".to_string(),
                Territory {
                    guild: GuildRef {
                        uuid: "old-uuid".to_string(),
                        name: "Other Guild".to_string(),
                        prefix: "OLD".to_string(),
                        color: Some((1, 2, 3)),
                    },
                    acquired: initial_acquired,
                    location: Region {
                        start: [0, 0],
                        end: [1, 1],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                    runtime: None,
                },
            );
        }

        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: vec![CanonicalTerritoryUpdate {
                territory: "Cinfras Outskirts".to_string(),
                guild: Some(GuildRef {
                    uuid: "new-uuid".to_string(),
                    name: "  AVICIA  ".to_string(),
                    prefix: "AVO".to_string(),
                    color: None,
                }),
                acquired: Some((initial_acquired + Duration::seconds(1)).to_rfc3339()),
                location: None,
                resources: None,
                connections: None,
                runtime: None,
                idempotency_key: None,
            }],
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("test-token"),
        );

        let response = ingest_territory(State(state.clone()), headers, Json(batch))
            .await
            .expect("ingest should accept valid internal token");
        let body = response.0;
        assert_eq!(body["applied"], 1);
        assert_eq!(body["rejected"], 0);

        let snapshot = state.live_snapshot.read().await;
        let updated = snapshot
            .territories
            .get("Cinfras Outskirts")
            .expect("territory should remain present");
        assert_eq!(updated.guild.color, Some((16, 16, 254)));
    }

    #[tokio::test]
    async fn ingest_rejects_invalid_internal_token() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("expected-token".to_string());

        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: Vec::new(),
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("wrong-token"),
        );

        let result = ingest_territory(State(state), headers, Json(batch)).await;
        assert!(matches!(result, Err(StatusCode::UNAUTHORIZED)));
    }

    #[tokio::test]
    async fn ingest_rejects_batches_over_configured_max() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("expected-token-that-is-long-enough".to_string());
        state.max_ingest_updates_per_request = 1;

        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: vec![
                CanonicalTerritoryUpdate {
                    territory: "Alpha".to_string(),
                    guild: None,
                    acquired: None,
                    location: None,
                    resources: None,
                    connections: None,
                    runtime: None,
                    idempotency_key: None,
                },
                CanonicalTerritoryUpdate {
                    territory: "Beta".to_string(),
                    guild: None,
                    acquired: None,
                    location: None,
                    resources: None,
                    connections: None,
                    runtime: None,
                    idempotency_key: None,
                },
            ],
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("expected-token-that-is-long-enough"),
        );

        let result = ingest_territory(State(state), headers, Json(batch)).await;
        assert!(matches!(result, Err(StatusCode::PAYLOAD_TOO_LARGE)));
    }

    #[tokio::test]
    async fn ownership_only_updates_do_not_reuse_stale_runtime_provenance_for_scalar_sampling() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("expected-token-that-is-long-enough".to_string());

        let initial_acquired = Utc::now();
        let observed_at = Utc::now().to_rfc3339();
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.territories.insert(
                "Alpha".to_string(),
                Territory {
                    guild: GuildRef {
                        uuid: "old-uuid".to_string(),
                        name: "Old Guild".to_string(),
                        prefix: "OLD".to_string(),
                        color: None,
                    },
                    acquired: initial_acquired,
                    location: Region {
                        start: [0, 0],
                        end: [1, 1],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                    runtime: Some(TerritoryRuntimeData {
                        provenance: Some(DataProvenance {
                            source: "fabric_reporter".to_string(),
                            visibility: VisibilityClass::Public,
                            confidence: 0.99,
                            reporter_count: 2,
                            observed_at: observed_at.clone(),
                            menu_season_id: Some(29),
                            menu_captured_territories: Some(64),
                            menu_sr_per_hour: Some(30301),
                            menu_observed_at: Some(observed_at),
                        }),
                        ..TerritoryRuntimeData::default()
                    }),
                },
            );
        }

        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: vec![CanonicalTerritoryUpdate {
                territory: "Alpha".to_string(),
                guild: Some(GuildRef {
                    uuid: "new-uuid".to_string(),
                    name: "New Guild".to_string(),
                    prefix: "NEW".to_string(),
                    color: None,
                }),
                acquired: Some((initial_acquired + Duration::seconds(1)).to_rfc3339()),
                location: None,
                resources: None,
                connections: None,
                runtime: None,
                idempotency_key: None,
            }],
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("expected-token-that-is-long-enough"),
        );

        let response = ingest_territory(State(state.clone()), headers, Json(batch))
            .await
            .expect("ingest should accept valid internal token");
        let body = response.0;
        assert_eq!(body["applied"], 1);
        assert_eq!(body["rejected"], 0);
        assert!(
            state.latest_scalar_sample.read().await.is_none(),
            "ownership-only update should not sample scalar from stale runtime provenance"
        );
    }

    #[tokio::test]
    async fn location_only_updates_bump_seq_refresh_live_state_and_emit_snapshot_event() {
        let mut state = AppState::new(None);
        state.internal_ingest_token = Some("expected-token-that-is-long-enough".to_string());

        let mut event_rx = state.event_tx.subscribe();
        let initial_acquired = Utc::now();
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.territories.insert(
                "Alpha".to_string(),
                Territory {
                    guild: GuildRef {
                        uuid: "guild-uuid".to_string(),
                        name: "Guild".to_string(),
                        prefix: "GLD".to_string(),
                        color: None,
                    },
                    acquired: initial_acquired,
                    location: Region {
                        start: [0, 0],
                        end: [1, 1],
                    },
                    resources: Default::default(),
                    connections: Vec::new(),
                    runtime: None,
                },
            );
        }

        let updated_location = Region {
            start: [5, 5],
            end: [6, 6],
        };
        let batch = CanonicalTerritoryBatch {
            generated_at: Utc::now().to_rfc3339(),
            updates: vec![CanonicalTerritoryUpdate {
                territory: "Alpha".to_string(),
                guild: None,
                acquired: None,
                location: Some(updated_location.clone()),
                resources: None,
                connections: None,
                runtime: None,
                idempotency_key: None,
            }],
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-internal-ingest-token",
            HeaderValue::from_static("expected-token-that-is-long-enough"),
        );

        let response = ingest_territory(State(state.clone()), headers, Json(batch))
            .await
            .expect("ingest should accept valid internal token");
        let latest_seq = response.0["latest_seq"]
            .as_u64()
            .expect("response should include latest_seq");
        assert!(latest_seq > 0);

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("snapshot event should be published")
            .expect("snapshot event should be readable");
        match event {
            PreSerializedEvent::Snapshot { seq, .. } => assert_eq!(seq, latest_seq),
            _ => panic!("location-only ingest should emit a snapshot event"),
        }

        let snapshot = state.live_snapshot.read().await;
        assert_eq!(snapshot.seq, latest_seq);
        assert_eq!(
            snapshot
                .territories
                .get("Alpha")
                .expect("territory should exist")
                .location,
            updated_location
        );

        let live_state: LiveState = serde_json::from_slice(snapshot.live_state_json.as_ref())
            .expect("live_state payload should deserialize");
        assert_eq!(live_state.seq, latest_seq);
        assert_eq!(
            live_state
                .territories
                .get("Alpha")
                .expect("territory should exist in live_state payload")
                .location,
            updated_location
        );
    }

    #[test]
    fn constant_time_eq_requires_exact_match() {
        assert!(constant_time_eq("token-value", "token-value"));
        assert!(!constant_time_eq("token-value", "token-valuf"));
        assert!(!constant_time_eq("short", "longer"));
    }

    #[test]
    fn scalar_duplicate_check_ignores_timestamp_and_requires_same_season() {
        let sample = SeasonScalarSample {
            sampled_at: "2026-02-28T20:00:00Z".to_string(),
            season_id: 29,
            scalar_weighted: 2.1456,
            scalar_raw: 3.1122,
            confidence: 0.99,
            sample_count: 1,
        };

        assert!(is_duplicate_scalar_sample(
            &sample, 29, 2.14565, 3.11218, 0.0005
        ));
        assert!(!is_duplicate_scalar_sample(
            &sample, 30, 2.14565, 3.11218, 0.0005
        ));
        assert!(!is_duplicate_scalar_sample(
            &sample, 29, 2.151, 3.11218, 0.0005
        ));
    }

    #[test]
    fn should_replace_override_prefers_newer_observed_at_even_with_lower_confidence() {
        let existing = IngestTerritoryOverride {
            observed_at: DateTime::parse_from_rfc3339("2026-03-02T21:00:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            confidence: 0.95,
            ..IngestTerritoryOverride::default()
        };
        let incoming = IngestTerritoryOverride {
            observed_at: DateTime::parse_from_rfc3339("2026-03-02T21:00:15Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            confidence: 0.70,
            ..IngestTerritoryOverride::default()
        };

        assert!(should_replace_ingest_override(Some(&existing), &incoming));
    }

    #[test]
    fn should_replace_override_rejects_older_observed_at_even_with_higher_confidence() {
        let existing = IngestTerritoryOverride {
            observed_at: DateTime::parse_from_rfc3339("2026-03-02T21:00:15Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            confidence: 0.70,
            ..IngestTerritoryOverride::default()
        };
        let incoming = IngestTerritoryOverride {
            observed_at: DateTime::parse_from_rfc3339("2026-03-02T21:00:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            confidence: 0.99,
            ..IngestTerritoryOverride::default()
        };

        assert!(!should_replace_ingest_override(Some(&existing), &incoming));
    }

    #[test]
    fn sanitize_override_observed_at_caps_far_future_timestamps() {
        let now = DateTime::parse_from_rfc3339("2026-03-02T21:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        let far_future = DateTime::parse_from_rfc3339("2026-03-02T21:05:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        let near_future = DateTime::parse_from_rfc3339("2026-03-02T21:00:10Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        assert_eq!(sanitize_override_observed_at(far_future, now), now);
        assert_eq!(sanitize_override_observed_at(near_future, now), near_future);
    }
}
