use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sequoia_shared::history::{HistoryBounds, HistoryGuildSrEntry, HistorySnapshot};
use sequoia_shared::{GuildRef, LiveState, SeasonScalarSample, Territory, TerritoryMap};

use crate::app::{BufferedUpdate, GuildColorMap, MapMode, TerritoryGeometryMap};
use crate::territory::{ClientTerritoryMap, apply_changes, from_snapshot};

const MAX_BUFFERED_UPDATES: usize = 20_000;
const REKINDLED_WORLD_RELEASE_SECS: i64 = 1_723_248_000; // 2024-08-10T00:00:00Z

thread_local! {
    static ACTIVE_HISTORY_FETCH: RefCell<Option<(u64, web_sys::AbortController)>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
pub struct HistoryFetchContext {
    pub mode: RwSignal<MapMode>,
    pub history_fetch_nonce: RwSignal<u64>,
    pub history_legacy_geometry_active: RwSignal<bool>,
    pub history_scalar_sample: RwSignal<Option<SeasonScalarSample>>,
    pub history_sr_leaderboard: RwSignal<Option<Vec<HistoryGuildSrEntry>>>,
    pub geo_store: StoredValue<TerritoryGeometryMap>,
    pub guild_color_store: StoredValue<GuildColorMap>,
    pub territories: RwSignal<ClientTerritoryMap>,
}

#[derive(Clone, Copy)]
pub struct HistoryStepContext {
    pub history_timestamp: RwSignal<Option<i64>>,
    pub playback_active: RwSignal<bool>,
    pub fetch: HistoryFetchContext,
}

/// Check if history features are available by querying /api/health.
pub fn check_availability(available: RwSignal<bool>) {
    check_availability_with_probe(available, None);
}

/// Check if history features are available by querying /api/health and optionally
/// increment a probe nonce when the request completes.
pub fn check_availability_with_probe(
    available: RwSignal<bool>,
    probe_nonce: Option<RwSignal<u64>>,
) {
    spawn_local(async move {
        let mut history_available = false;

        if let Ok(resp) = gloo_net::http::Request::get("/api/health").send().await {
            if resp.ok() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    history_available = json
                        .get("history_available")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                }
            }
        }

        if history_available {
            available.set(true);
        }

        if let Some(probe_nonce) = probe_nonce {
            probe_nonce.update(|n| *n = n.wrapping_add(1));
        }
    });
}

async fn fetch_history_at_with_abort(
    timestamp_secs: i64,
    abort_signal: Option<&web_sys::AbortSignal>,
) -> Result<HistorySnapshot, String> {
    let dt = chrono::DateTime::from_timestamp(timestamp_secs, 0).ok_or("invalid timestamp")?;
    let t = dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let url = format!("/api/history/at?t={}", t);

    let resp = gloo_net::http::Request::get(&url)
        .abort_signal(abort_signal)
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<HistorySnapshot>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

fn replace_active_history_fetch(request_nonce: u64, next: Option<web_sys::AbortController>) {
    ACTIVE_HISTORY_FETCH.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((_, current)) = slot.take() {
            current.abort();
        }
        if let Some(controller) = next {
            *slot = Some((request_nonce, controller));
        }
    });
}

fn clear_active_history_fetch(request_nonce: u64) {
    ACTIVE_HISTORY_FETCH.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(active_nonce, _)| *active_nonce == request_nonce)
        {
            slot.take();
        }
    });
}

fn abort_active_history_fetch() {
    replace_active_history_fetch(0, None);
}

fn is_abort_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("aborterror") || error.contains("aborted")
}

/// Fetch the current live territory snapshot.
pub async fn fetch_live_snapshot() -> Result<TerritoryMap, String> {
    let resp = gloo_net::http::Request::get("/api/territories")
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<TerritoryMap>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

/// Fetch a gap-free live snapshot with sequence.
pub async fn fetch_live_state() -> Result<LiveState, String> {
    let resp = gloo_net::http::Request::get("/api/live/state")
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<LiveState>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

/// Fetch history bounds from the API.
pub async fn fetch_bounds() -> Result<HistoryBounds, String> {
    let resp = gloo_net::http::Request::get("/api/history/bounds")
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<HistoryBounds>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

/// Merge historical ownership data with immutable territory geometry.
/// Territories not found in the geometry store are skipped (no zero-region fallback).
pub fn merge_with_static(
    snapshot: &HistorySnapshot,
    geometry: &TerritoryGeometryMap,
    guild_colors: &GuildColorMap,
) -> TerritoryMap {
    let mut map = TerritoryMap::new();

    for (name, record) in &snapshot.ownership {
        let Some((location, resources, connections)) = geometry.get(name) else {
            continue;
        };

        let acquired = parse_history_time(&record.acquired_at).unwrap_or_else(chrono::Utc::now);

        map.insert(
            name.clone(),
            Territory {
                guild: GuildRef {
                    uuid: record.guild_uuid.clone(),
                    name: record.guild_name.clone(),
                    prefix: record.guild_prefix.clone(),
                    color: record
                        .guild_color
                        .or_else(|| guild_colors.get(&record.guild_name).copied()),
                },
                acquired,
                location: location.clone(),
                resources: resources.clone(),
                connections: connections.clone(),
                runtime: None,
            },
        );
    }

    map
}

/// Fetch historical data and update the territories signal.
/// Reads geometry from `TerritoryGeometryStore` context.
pub fn fetch_and_apply_with(timestamp_secs: i64, ctx: HistoryFetchContext) {
    let HistoryFetchContext {
        mode,
        history_fetch_nonce: fetch_nonce,
        history_legacy_geometry_active,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
    } = ctx;

    let request_nonce = fetch_nonce.get_untracked().wrapping_add(1);
    fetch_nonce.set(request_nonce);
    history_legacy_geometry_active.set(timestamp_uses_legacy_geometry(timestamp_secs));
    let abort_controller = web_sys::AbortController::new().ok();
    let abort_signal = abort_controller
        .as_ref()
        .map(|controller| controller.signal());
    replace_active_history_fetch(request_nonce, abort_controller);

    spawn_local(async move {
        match fetch_history_at_with_abort(timestamp_secs, abort_signal.as_ref()).await {
            Ok(snapshot) => {
                clear_active_history_fetch(request_nonce);
                // Only apply the latest in-flight history fetch, and only while still in history mode.
                if fetch_nonce.get_untracked() != request_nonce
                    || mode.get_untracked() != MapMode::History
                {
                    return;
                }
                history_scalar_sample.set(snapshot.season_scalar.clone());
                history_sr_leaderboard.set(snapshot.season_leaderboard.clone());
                let geo = geo_store.get_value();
                let guild_colors = guild_color_store.get_value();
                let merged = merge_with_static(&snapshot, &geo, &guild_colors);
                territories.set(from_snapshot(merged));
            }
            Err(e) => {
                clear_active_history_fetch(request_nonce);
                if is_abort_error(&e) {
                    return;
                }
                if fetch_nonce.get_untracked() != request_nonce {
                    return;
                }
                web_sys::console::warn_1(&format!("History fetch failed: {e}").into());
            }
        }
    });
}

/// Buffer one incoming live update while history mode is active.
pub fn buffer_history_update(
    history_buffered_updates: RwSignal<Vec<BufferedUpdate>>,
    history_buffer_size_max: RwSignal<usize>,
    needs_live_resync: RwSignal<bool>,
    update: BufferedUpdate,
) {
    let mut overflowed = false;
    let mut new_len = 0;

    history_buffered_updates.update(|buffer| {
        if buffer.iter().any(|existing| existing.seq == update.seq) {
            new_len = buffer.len();
            return;
        }

        buffer.push(update);
        buffer.sort_by_key(|item| item.seq);

        if buffer.len() > MAX_BUFFERED_UPDATES {
            let overflow = buffer.len() - MAX_BUFFERED_UPDATES;
            buffer.drain(0..overflow);
            overflowed = true;
        }

        new_len = buffer.len();
    });

    if overflowed {
        needs_live_resync.set(true);
        web_sys::console::warn_1(
            &"history buffer overflowed; forcing live resync on handoff".into(),
        );
    }

    let mut updated_max = None;
    history_buffer_size_max.update(|current_max| {
        if new_len > *current_max {
            *current_max = new_len;
            updated_max = Some(new_len);
        }
    });
    if let Some(max_size) = updated_max {
        web_sys::console::info_1(&format!("history_buffer_size_max={max_size}").into());
    }
}

pub fn replay_updates_after_seq(
    baseline_seq: u64,
    buffered_updates: &[BufferedUpdate],
) -> Vec<BufferedUpdate> {
    let mut ordered = buffered_updates.to_vec();
    ordered.sort_by_key(|item| item.seq);

    let mut replay = Vec::new();
    let mut last_seen_seq = baseline_seq;

    for update in ordered {
        if update.seq <= baseline_seq || update.seq == last_seen_seq {
            continue;
        }
        last_seen_seq = update.seq;
        replay.push(update);
    }

    replay
}

pub fn has_seq_gap(last_live_seq: Option<u64>, incoming_seq: u64) -> bool {
    if incoming_seq == 0 {
        return false;
    }

    match last_live_seq {
        Some(last_seq) => incoming_seq != last_seq.saturating_add(1),
        None => false,
    }
}

#[derive(Clone, Copy)]
pub struct EnterHistoryModeInput {
    pub mode: RwSignal<MapMode>,
    pub history_timestamp: RwSignal<Option<i64>>,
    pub history_bounds: RwSignal<Option<(i64, i64)>>,
    pub history_fetch_nonce: RwSignal<u64>,
    pub history_legacy_geometry_active: RwSignal<bool>,
    pub history_buffered_updates: RwSignal<Vec<BufferedUpdate>>,
    pub history_buffer_mode_active: RwSignal<bool>,
    pub needs_live_resync: RwSignal<bool>,
    pub history_scalar_sample: RwSignal<Option<SeasonScalarSample>>,
    pub history_sr_leaderboard: RwSignal<Option<Vec<HistoryGuildSrEntry>>>,
    pub geo_store: StoredValue<TerritoryGeometryMap>,
    pub guild_color_store: StoredValue<GuildColorMap>,
    pub territories: RwSignal<ClientTerritoryMap>,
}

/// Enter history mode: set mode, fetch bounds, and set initial timestamp to now.
/// Captures a snapshot of live territory geometry for use throughout the history session.
/// If bounds fetch fails (e.g. 503 — no database), automatically exits history mode.
pub fn enter_history_mode(input: EnterHistoryModeInput) {
    let EnterHistoryModeInput {
        mode,
        history_timestamp,
        history_bounds,
        history_fetch_nonce,
        history_legacy_geometry_active,
        history_buffered_updates,
        history_buffer_mode_active,
        needs_live_resync,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
    } = input;

    abort_active_history_fetch();
    history_fetch_nonce.update(|n| *n = n.wrapping_add(1));
    history_buffer_mode_active.set(true);
    history_buffered_updates.set(Vec::new());
    needs_live_resync.set(false);
    history_scalar_sample.set(None);
    history_sr_leaderboard.set(None);

    let now = chrono::Utc::now().timestamp();
    history_timestamp.set(Some(now));
    history_legacy_geometry_active.set(timestamp_uses_legacy_geometry(now));
    mode.set(MapMode::History);

    // History currently reuses the live geometry snapshot for the full session.
    // Older pre-Rekindled timestamps still need dedicated geometry/assets.
    let live = territories.get_untracked();
    let geo: TerritoryGeometryMap = live
        .iter()
        .map(|(name, ct)| {
            (
                name.clone(),
                (
                    ct.territory.location.clone(),
                    ct.territory.resources.clone(),
                    ct.territory.connections.clone(),
                ),
            )
        })
        .collect();
    let colors: GuildColorMap = live
        .values()
        .filter_map(|ct| {
            ct.territory
                .guild
                .color
                .map(|color| (ct.territory.guild.name.clone(), color))
        })
        .collect();
    geo_store.set_value(geo);
    guild_color_store.set_value(colors);

    // Fetch bounds — exit history mode on failure
    spawn_local(async move {
        match fetch_bounds().await {
            Ok(bounds) => {
                let earliest = bounds
                    .earliest
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.timestamp())
                    .unwrap_or(now - 86400);
                let latest = bounds
                    .latest
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.timestamp())
                    .unwrap_or(now);
                history_bounds.set(Some((earliest, latest)));
                // Fetch initial historical data so the map updates immediately
                fetch_and_apply_with(
                    now,
                    HistoryFetchContext {
                        mode,
                        history_fetch_nonce,
                        history_legacy_geometry_active,
                        history_scalar_sample,
                        history_sr_leaderboard,
                        geo_store,
                        guild_color_store,
                        territories,
                    },
                );
            }
            Err(_) => {
                // Server doesn't support history — exit back to live mode
                history_buffer_mode_active.set(false);
                history_buffered_updates.set(Vec::new());
                history_legacy_geometry_active.set(false);
                mode.set(MapMode::Live);
                history_timestamp.set(None);
            }
        }
    });
}

#[derive(Clone, Copy)]
pub struct ExitHistoryModeInput {
    pub mode: RwSignal<MapMode>,
    pub playback_active: RwSignal<bool>,
    pub history_fetch_nonce: RwSignal<u64>,
    pub history_timestamp: RwSignal<Option<i64>>,
    pub history_legacy_geometry_active: RwSignal<bool>,
    pub history_buffered_updates: RwSignal<Vec<BufferedUpdate>>,
    pub history_buffer_mode_active: RwSignal<bool>,
    pub last_live_seq: RwSignal<Option<u64>>,
    pub needs_live_resync: RwSignal<bool>,
    pub live_handoff_resync_count: RwSignal<u64>,
    pub history_sr_leaderboard: RwSignal<Option<Vec<HistoryGuildSrEntry>>>,
    pub territories: RwSignal<ClientTerritoryMap>,
}

/// Exit history mode with a gap-free handoff to live.
pub fn exit_history_mode(input: ExitHistoryModeInput) {
    let ExitHistoryModeInput {
        mode,
        playback_active,
        history_fetch_nonce,
        history_timestamp,
        history_legacy_geometry_active,
        history_buffered_updates,
        history_buffer_mode_active,
        last_live_seq,
        needs_live_resync,
        live_handoff_resync_count,
        history_sr_leaderboard,
        territories,
    } = input;

    let request_nonce = history_fetch_nonce.get_untracked().wrapping_add(1);
    history_fetch_nonce.set(request_nonce);
    abort_active_history_fetch();
    playback_active.set(false);

    let mut handoff_count = 0;
    live_handoff_resync_count.update(|count| {
        *count = count.saturating_add(1);
        handoff_count = *count;
    });
    web_sys::console::info_1(&format!("live_handoff_resync_count={handoff_count}").into());

    spawn_local(async move {
        let live_state = fetch_live_state().await;

        // Still in this exact history session?
        if history_fetch_nonce.get_untracked() != request_nonce
            || mode.get_untracked() != MapMode::History
        {
            return;
        }

        match live_state {
            Ok(state) => {
                let mut newest_seq = state.seq;
                territories.set(from_snapshot(state.territories));

                let replay =
                    replay_updates_after_seq(state.seq, &history_buffered_updates.get_untracked());

                if !replay.is_empty() {
                    let now = js_sys::Date::now();
                    territories.update(|map| {
                        for update in &replay {
                            apply_changes(map, &update.changes, now, 800.0);
                        }
                    });
                    if let Some(max_seq) = replay.iter().map(|u| u.seq).max() {
                        newest_seq = newest_seq.max(max_seq);
                    }
                }

                history_buffered_updates.set(Vec::new());
                history_buffer_mode_active.set(false);
                needs_live_resync.set(false);
                history_timestamp.set(None);
                history_legacy_geometry_active.set(false);
                history_sr_leaderboard.set(None);
                last_live_seq.set(Some(newest_seq));
                mode.set(MapMode::Live);
            }
            Err(e) => {
                web_sys::console::warn_1(
                    &format!("Live-state handoff failed, falling back to snapshot: {e}").into(),
                );

                // Backward-compatibility fallback while mixed client/server versions exist.
                if let Ok(snapshot) = fetch_live_snapshot().await {
                    if history_fetch_nonce.get_untracked() != request_nonce
                        || mode.get_untracked() != MapMode::History
                    {
                        return;
                    }
                    territories.set(from_snapshot(snapshot));
                }

                history_buffered_updates.set(Vec::new());
                history_buffer_mode_active.set(false);
                history_timestamp.set(None);
                history_legacy_geometry_active.set(false);
                history_sr_leaderboard.set(None);
                last_live_seq.set(None);
                needs_live_resync.set(true);
                mode.set(MapMode::Live);
            }
        }
    });
}

/// Step backward by 60 seconds.
pub fn step_backward(ctx: HistoryStepContext) {
    step_time(ctx, -60);
}

/// Step forward by 60 seconds.
pub fn step_forward(ctx: HistoryStepContext) {
    step_time(ctx, 60);
}

fn step_time(ctx: HistoryStepContext, delta_secs: i64) {
    ctx.playback_active.set(false);
    ctx.history_timestamp.update(|ts| {
        if let Some(t) = ts {
            *t += delta_secs;
        }
    });
    if let Some(ts) = ctx.history_timestamp.get_untracked() {
        fetch_and_apply_with(ts, ctx.fetch);
    }
}

fn parse_history_time(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&chrono::Utc));
    }

    // Accept PostgreSQL text format seen in history API: "YYYY-MM-DD HH:MM:SS.sss+00"
    let mut normalized = raw.replace(' ', "T");
    if normalized.len() >= 3 {
        let tail = &normalized[normalized.len() - 3..];
        let tail_bytes = tail.as_bytes();
        if (tail_bytes[0] == b'+' || tail_bytes[0] == b'-')
            && tail_bytes[1].is_ascii_digit()
            && tail_bytes[2].is_ascii_digit()
        {
            normalized.push_str(":00");
        }
    }
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

pub fn rekindled_world_release_secs() -> i64 {
    REKINDLED_WORLD_RELEASE_SECS
}

pub fn timestamp_uses_legacy_geometry(timestamp_secs: i64) -> bool {
    timestamp_secs < REKINDLED_WORLD_RELEASE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered(seq: u64) -> BufferedUpdate {
        BufferedUpdate {
            seq,
            changes: Vec::new(),
        }
    }

    #[test]
    fn replay_filters_to_updates_after_baseline_seq() {
        let buffer = vec![
            buffered(4),
            buffered(2),
            buffered(8),
            buffered(8),
            buffered(5),
        ];
        let replay = replay_updates_after_seq(4, &buffer);
        let seqs: Vec<u64> = replay.into_iter().map(|u| u.seq).collect();
        assert_eq!(seqs, vec![5, 8]);
    }

    #[test]
    fn detects_sequence_gap() {
        assert!(!has_seq_gap(Some(10), 11));
        assert!(has_seq_gap(Some(10), 12));
        assert!(!has_seq_gap(None, 7));
    }

    #[test]
    fn flags_pre_rekindled_timestamps_as_legacy_geometry() {
        assert!(timestamp_uses_legacy_geometry(
            REKINDLED_WORLD_RELEASE_SECS - 1
        ));
        assert!(!timestamp_uses_legacy_geometry(
            REKINDLED_WORLD_RELEASE_SECS
        ));
    }
}
