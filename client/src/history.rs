use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sequoia_browser_map::live_feed::fetch_live_state;
use sequoia_shared::history::{
    HistoryBounds, HistoryGuildSrEntry, HistorySnapshot, HistoryTerritoryState,
};
use sequoia_shared::{
    GuildRef, Region, Resources, SeasonScalarSample, Territory, TerritoryMap, TerritoryRuntimeData,
};

use crate::app::{BufferedUpdate, GuildColorMap, MapMode, TerritoryGeometry, TerritoryGeometryMap};
use crate::territory::{ClientTerritory, ClientTerritoryMap, apply_changes, from_snapshot};

const REKINDLED_WORLD_RELEASE_SECS: i64 = 1_723_248_000; // 2024-08-10T00:00:00Z
const SEEK_RETRY_MS: u32 = 2_000;
/// The shortest gap between two seeks that follow a drag.
const PREVIEW_INTERVAL_MS: f64 = 100.0;

thread_local! {
    static ACTIVE_HISTORY_FETCH: RefCell<Option<(u64, web_sys::AbortController)>> = const { RefCell::new(None) };
    /// Changes on every entry to and exit from history, so a reply that belongs to an
    /// earlier visit can never change the map.
    static HISTORY_SESSION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PREVIEW: RefCell<Preview> = const { RefCell::new(Preview {
        in_flight: false,
        pending: None,
        last_sent_ms: f64::NEG_INFINITY,
        timer: None,
    }) };
}

/// The seeks that follow a drag. One is in flight at a time and is never aborted by the next
/// input, so every reply that lands moves the map; inputs meanwhile only replace `pending`.
struct Preview {
    in_flight: bool,
    /// The newest dragged-to time not yet requested, with the history visit it belongs to.
    pending: Option<(i64, u64)>,
    last_sent_ms: f64,
    timer: Option<gloo_timers::callback::Timeout>,
}

pub fn current_session() -> u64 {
    HISTORY_SESSION.with(std::cell::Cell::get)
}

fn begin_session() -> u64 {
    HISTORY_SESSION.with(|session| {
        session.set(session.get().wrapping_add(1));
        session.get()
    })
}

#[derive(Clone, Copy)]
pub struct HistoryFetchContext {
    /// The instant the drawn territories show; their clocks count against it.
    pub history_map_time: RwSignal<Option<i64>>,
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

        if let Ok(resp) = gloo_net::http::Request::get("/api/health").send().await
            && resp.ok()
            && let Ok(json) = resp.json::<serde_json::Value>().await
        {
            history_available = json
                .get("history_available")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
    let url = format!("/api/history/at?t={t}");

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
    cancel_preview();
    replace_active_history_fetch(0, None);
}

/// Show `timestamp` while a slider is being dragged: as soon as the seek in flight lands and
/// at most every [`PREVIEW_INTERVAL_MS`]. Releasing the slider seeks exactly with
/// [`fetch_and_apply_with`].
pub fn preview(timestamp_secs: i64, ctx: HistoryFetchContext) {
    PREVIEW
        .with(|preview| preview.borrow_mut().pending = Some((timestamp_secs, current_session())));
    send_preview(ctx);
}

fn send_preview(ctx: HistoryFetchContext) {
    let next = PREVIEW.with(|preview| {
        let mut preview = preview.borrow_mut();
        if preview.in_flight || preview.timer.is_some() {
            return None;
        }
        let (timestamp_secs, session) = preview.pending?;
        if session != current_session() || ctx.mode.get_untracked() != MapMode::History {
            preview.pending = None;
            return None;
        }
        let now = js_sys::Date::now();
        let wait = PREVIEW_INTERVAL_MS - (now - preview.last_sent_ms);
        if wait > 0.0 {
            preview.timer = Some(gloo_timers::callback::Timeout::new(
                wait.ceil() as u32,
                move || {
                    if let Some(timer) = PREVIEW.with(|preview| preview.borrow_mut().timer.take()) {
                        timer.forget();
                    }
                    send_preview(ctx);
                },
            ));
            return None;
        }
        preview.pending = None;
        preview.in_flight = true;
        preview.last_sent_ms = now;
        Some(timestamp_secs)
    });
    if let Some(timestamp_secs) = next {
        seek(timestamp_secs, ctx, move || {
            PREVIEW.with(|preview| preview.borrow_mut().in_flight = false);
            send_preview(ctx);
        });
    }
}

fn cancel_preview() {
    PREVIEW.with(|preview| {
        let mut preview = preview.borrow_mut();
        preview.pending = None;
        if let Some(timer) = preview.timer.take() {
            timer.cancel();
        }
    });
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

/// A territory's map parts at a historical time, for the holding (guild UUID, acquisition in
/// microseconds) shown then. Its recorded official-API state supplies them, the runtime only
/// when the API reported that same holding; without one, today's geometry stands in and there
/// is no runtime, so nothing live leaks into history. `None` when neither knows where the
/// territory is.
pub(crate) fn historical_parts(
    geometry: Option<&TerritoryGeometry>,
    recorded: Option<&HistoryTerritoryState>,
    holding: (&str, i64),
) -> Option<(Region, Resources, Vec<String>, Option<TerritoryRuntimeData>)> {
    let Some(recorded) = recorded else {
        let (location, resources, connections) = geometry?;
        return Some((
            location.clone(),
            resources.clone(),
            connections.clone(),
            None,
        ));
    };
    let state = &recorded.state;
    let location = state
        .location
        .clone()
        .or_else(|| geometry.map(|(location, ..)| location.clone()))?;
    let resources = if state.base_generation.is_some() {
        state.resources()
    } else {
        geometry.map_or_else(Resources::default, |(_, resources, _)| resources.clone())
    };
    let connections = state
        .links
        .clone()
        .or_else(|| geometry.map(|(.., connections)| connections.clone()))
        .unwrap_or_default();
    let runtime = state
        .reported_for(holding.0, holding.1)
        .then(|| state.runtime(recorded.observed_at.clone()))
        .flatten();
    Some((location, resources, connections, runtime))
}

/// Merge historical ownership and recorded state with immutable territory geometry.
/// Territories neither the state nor the geometry store places are skipped (no zero-region
/// fallback).
pub fn merge_with_static(
    snapshot: &HistorySnapshot,
    geometry: &TerritoryGeometryMap,
    guild_colors: &GuildColorMap,
) -> TerritoryMap {
    let mut map = TerritoryMap::new();

    for (name, record) in &snapshot.ownership {
        let acquired = parse_history_time(&record.acquired_at).unwrap_or_else(chrono::Utc::now);
        let Some((location, resources, connections, runtime)) = historical_parts(
            geometry.get(name),
            snapshot.states.get(name),
            (&record.guild_uuid, acquired.timestamp_micros()),
        ) else {
            continue;
        };

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
                location,
                resources,
                connections,
                runtime,
            },
        );
    }

    map
}

/// Replace the map with `merged`, rebuilding only territories whose state differs (owner,
/// acquisition, recorded state, or a live runtime a historical state does not carry), and
/// leaving the signal untouched when nothing does: scrubbing through a quiet interval then
/// costs no map rebuild.
fn apply_ownership(territories: RwSignal<ClientTerritoryMap>, mut merged: TerritoryMap) {
    let changed = territories.with_untracked(|current| {
        current.len() != merged.len()
            || merged.iter().any(|(name, territory)| {
                current
                    .get(name)
                    .is_none_or(|existing| existing.territory != *territory)
            })
    });
    if !changed {
        return;
    }
    territories.update(|current| {
        current.retain(|name, _| merged.contains_key(name));
        for (name, territory) in merged.drain() {
            if current
                .get(&name)
                .is_none_or(|existing| existing.territory != territory)
            {
                let client = ClientTerritory::from_territory(&name, territory);
                current.insert(name, client);
            }
        }
    });
}

/// Fetch historical data and update the territories signal.
/// Reads geometry from `TerritoryGeometryStore` context.
pub fn fetch_and_apply_with(timestamp_secs: i64, ctx: HistoryFetchContext) {
    cancel_preview();
    seek(timestamp_secs, ctx, || {});
}

/// Seek to `timestamp_secs`, replacing any seek in flight; `settled` runs once the request
/// has ended, however it ended.
fn seek(timestamp_secs: i64, ctx: HistoryFetchContext, settled: impl FnOnce() + 'static) {
    let HistoryFetchContext {
        history_fetch_nonce: fetch_nonce,
        history_legacy_geometry_active,
        ..
    } = ctx;

    let request_nonce = fetch_nonce.get_untracked().wrapping_add(1);
    fetch_nonce.set(request_nonce);
    let session = current_session();
    crate::playback::restart_at(timestamp_secs, request_nonce);
    history_legacy_geometry_active.set(timestamp_uses_legacy_geometry(timestamp_secs));
    let abort_controller = web_sys::AbortController::new().ok();
    let abort_signal = abort_controller
        .as_ref()
        .map(|controller| controller.signal());
    replace_active_history_fetch(request_nonce, abort_controller);

    spawn_local(async move {
        let fetched = fetch_history_at_with_abort(timestamp_secs, abort_signal.as_ref()).await;
        clear_active_history_fetch(request_nonce);
        apply_seek(fetched, timestamp_secs, request_nonce, session, ctx);
        settled();
    });
}

fn apply_seek(
    fetched: Result<HistorySnapshot, String>,
    timestamp_secs: i64,
    request_nonce: u64,
    session: u64,
    ctx: HistoryFetchContext,
) {
    let HistoryFetchContext {
        history_map_time,
        mode,
        history_fetch_nonce: fetch_nonce,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
        ..
    } = ctx;
    match fetched {
        Ok(snapshot) => {
            // Only the latest seek of this history visit may change the map.
            if fetch_nonce.get_untracked() != request_nonce
                || current_session() != session
                || mode.get_untracked() != MapMode::History
            {
                return;
            }
            let merged = geo_store.with_value(|geo| {
                guild_color_store.with_value(|colors| merge_with_static(&snapshot, geo, colors))
            });
            history_scalar_sample.set(snapshot.season_scalar);
            history_sr_leaderboard.set(snapshot.season_leaderboard);
            apply_ownership(territories, merged);
            history_map_time.set(Some(timestamp_secs));
            crate::playback::seek_applied(request_nonce, snapshot.states);
        }
        Err(e) => {
            if is_abort_error(&e) {
                return;
            }
            let current = move || {
                fetch_nonce.get_untracked() == request_nonce
                    && current_session() == session
                    && mode.get_untracked() == MapMode::History
            };
            if !current() {
                return;
            }
            web_sys::console::warn_1(&format!("History fetch failed: {e}").into());
            // Playback waits for this seek, so retry it unless a newer seek, an exit or
            // another visit has replaced it by then.
            gloo_timers::callback::Timeout::new(SEEK_RETRY_MS, move || {
                if current() {
                    fetch_and_apply_with(timestamp_secs, ctx);
                }
            })
            .forget();
        }
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

#[derive(Clone, Copy)]
pub struct EnterHistoryModeInput {
    pub history_map_time: RwSignal<Option<i64>>,
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
        history_map_time,
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
    let session = begin_session();
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
        let bounds = fetch_bounds().await;
        if current_session() != session || mode.get_untracked() != MapMode::History {
            return;
        }
        match bounds {
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
                        history_map_time,
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
    let session = begin_session();
    crate::playback::stop();
    playback_active.set(false);

    let mut handoff_count = 0;
    live_handoff_resync_count.update(|count| {
        *count = count.saturating_add(1);
        handoff_count = *count;
    });
    web_sys::console::info_1(&format!("live_handoff_resync_count={handoff_count}").into());
    // The live state fetched below supersedes every event dropped so far; one dropped while
    // it is in flight may be newer than it and must still force a resync.
    needs_live_resync.set(false);

    spawn_local(async move {
        let live_state = fetch_live_state().await;

        // Still leaving this exact history visit?
        if history_fetch_nonce.get_untracked() != request_nonce
            || current_session() != session
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
        let tail_bytes = &normalized.as_bytes()[normalized.len() - 3..];
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
    fn a_seek_drops_live_runtime_from_a_territory_whose_owner_is_unchanged() {
        let owner = Owner::new();
        owner.set();
        let live: Territory = serde_json::from_value(serde_json::json!({
            "guild": {"uuid": "g", "name": "Guild", "prefix": "G", "color": [1, 2, 3]},
            "acquired": "2026-01-01T00:00:00Z",
            "location": {"start": [0, 0], "end": [10, 10]},
            "runtime": {"defense_tier": "Very High", "headquarters": true}
        }))
        .expect("territory");
        let mut historical = live.clone();
        historical.runtime = None;
        let territories = RwSignal::new(ClientTerritoryMap::from([(
            "A".to_string(),
            ClientTerritory::from_territory("A", live),
        )]));

        apply_ownership(
            territories,
            TerritoryMap::from([("A".to_string(), historical)]),
        );

        assert_eq!(territories.get_untracked()["A"].territory.runtime, None);
    }

    #[test]
    fn history_time_accepts_rfc3339_and_postgres_offsets() {
        let expected = chrono::DateTime::parse_from_rfc3339("2026-01-01T12:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for input in [
            "2026-01-01T12:30:00Z",
            "2026-01-01 12:30:00+00",
            "2026-01-01 14:30:00+02",
        ] {
            assert_eq!(parse_history_time(input), Some(expected));
        }
    }

    #[test]
    fn a_seek_shows_recorded_state_only_for_the_holding_it_was_reported_for() {
        let state: HistoryTerritoryState = serde_json::from_value(serde_json::json!({
            "observed_at": "2026-11-02T11:17:44+00:00",
            "state": {
                "owner": {"guild_uuid": "g", "acquired_micros": 1_767_225_600_000_000_i64},
                "headquarters": true, "guild_headquarters": "A", "treasury": "LOW",
                "defences": "VERY_HIGH",
                "generation": {"emeralds": 9000, "ore": 0, "crops": 0, "fish": 0, "wood": 0},
                "base_generation": {"emeralds": 9000, "ore": 3600, "crops": 0, "fish": 0, "wood": 0},
                "stored": {"emeralds": 120, "ore": 0, "crops": 0, "fish": 0, "wood": 0},
                "capacity": null, "location": {"start": [5, 5], "end": [15, 15]},
                "links": ["B"]
            }
        }))
        .expect("state");
        let snapshot = |guild_uuid: &str| HistorySnapshot {
            timestamp: "2026-11-02T12:00:00Z".to_string(),
            ownership: std::collections::HashMap::from([(
                "A".to_string(),
                sequoia_shared::history::OwnershipRecord {
                    guild_uuid: guild_uuid.to_string(),
                    guild_name: "Guild".to_string(),
                    guild_prefix: "G".to_string(),
                    guild_color: None,
                    acquired_at: "2026-01-01T00:00:00Z".to_string(),
                },
            )]),
            season_scalar: None,
            season_leaderboard: None,
            states: std::collections::HashMap::from([("A".to_string(), state.clone())]),
        };
        let geometry = TerritoryGeometryMap::from([(
            "A".to_string(),
            (
                Region {
                    start: [0, 0],
                    end: [10, 10],
                },
                Resources::default(),
                Vec::new(),
            ),
        )]);

        let same = merge_with_static(&snapshot("g"), &geometry, &GuildColorMap::new());
        let runtime = same["A"].runtime.clone().expect("recorded runtime");
        assert_eq!(runtime.headquarters, Some(true));
        assert_eq!(runtime.defense_tier.as_deref(), Some("VERY HIGH"));
        assert_eq!(runtime.held_resources.map(|held| held.emeralds), Some(120));
        assert_eq!(
            runtime.provenance.map(|p| p.observed_at),
            Some(state.observed_at.clone())
        );
        assert_eq!(
            same["A"].location.start,
            [5, 5],
            "the recorded location wins"
        );
        assert_eq!(same["A"].connections, vec!["B".to_string()]);

        let produced = Resources {
            emeralds: 9000,
            ..Resources::default()
        };
        assert_eq!(same["A"].resources, produced);

        let other = merge_with_static(&snapshot("h"), &geometry, &GuildColorMap::new());
        assert_eq!(
            other["A"].runtime, None,
            "another guild's holding shows none of it"
        );
        assert_eq!(other["A"].resources, produced, "ore is not produced here");
        assert_eq!(other["A"].location.start, [5, 5]);
    }

    #[test]
    fn history_time_rejects_malformed_unicode_without_panicking() {
        for input in ["", "é", "🙂", "invalid🙂", "2026-01-01 12:30:00é"] {
            assert_eq!(parse_history_time(input), None);
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
    fn flags_pre_rekindled_timestamps_as_legacy_geometry() {
        assert!(timestamp_uses_legacy_geometry(
            REKINDLED_WORLD_RELEASE_SECS - 1
        ));
        assert!(!timestamp_uses_legacy_geometry(
            REKINDLED_WORLD_RELEASE_SECS
        ));
    }
}
