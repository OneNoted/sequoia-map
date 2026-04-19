use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sequoia_shared::history::{HistoryEvents, HistorySrSamples, HistorySrSnapshot};

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBoundsSignal, HistoryFetchNonce,
    HistoryLegacyGeometryActive, HistorySeasonLeaderboard, HistorySeasonScalarSample,
    HistoryTimestamp, MapMode, PlaybackActive, PlaybackSpeed, TerritoryGeometryStore,
};
use crate::territory::{ClientTerritoryMap, apply_changes};

struct PlaybackIntervalBinding {
    window: web_sys::Window,
    interval_id: i32,
    _callback: Closure<dyn Fn()>,
}

thread_local! {
    static PLAYBACK_INTERVAL_BINDING: RefCell<Option<PlaybackIntervalBinding>> = const { RefCell::new(None) };
}

fn parse_rfc3339_secs(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Fetch upcoming events from the history API.
async fn fetch_events(
    from_secs: i64,
    to_secs: i64,
    after_seq: Option<u64>,
    limit: i64,
) -> Result<HistoryEvents, String> {
    let from_dt = chrono::DateTime::from_timestamp(from_secs, 0).ok_or("invalid from timestamp")?;
    let to_dt = chrono::DateTime::from_timestamp(to_secs, 0).ok_or("invalid to timestamp")?;

    let mut url = format!(
        "/api/history/events?from={}&to={}&limit={}",
        from_dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        to_dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        limit
    );
    if let Some(after_seq) = after_seq {
        url.push_str(&format!("&after_seq={after_seq}"));
    }

    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<HistoryEvents>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

/// Fetch season rating snapshots for the given window.
async fn fetch_sr_samples(from_secs: i64, to_secs: i64) -> Result<Vec<HistorySrSnapshot>, String> {
    let from_dt = chrono::DateTime::from_timestamp(from_secs, 0).ok_or("invalid from timestamp")?;
    let to_dt = chrono::DateTime::from_timestamp(to_secs, 0).ok_or("invalid to timestamp")?;
    let url = format!(
        "/api/history/sr-samples?from={}&to={}",
        from_dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        to_dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );

    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<HistorySrSamples>()
        .await
        .map(|payload| payload.samples)
        .map_err(|e| format!("parse error: {e}"))
}

/// Returns an animation duration (ms) scaled to the current playback speed.
/// Faster playback = shorter or no transition to avoid animation pile-up.
fn playback_animation_duration(speed: f64) -> f64 {
    if speed >= 360.0 {
        0.0 // instant
    } else if speed >= 60.0 {
        50.0 // minimal
    } else if speed >= 10.0 {
        100.0 // brief
    } else {
        200.0 // visible but snappy
    }
}

/// Starts the playback engine. Call this once from an Effect.
/// The interval runs at 100ms but only advances time when in history mode and playing.
pub fn start_playback_engine() {
    PLAYBACK_INTERVAL_BINDING.with(|slot| {
        if let Some(old) = slot.borrow_mut().take() {
            old.window.clear_interval_with_handle(old.interval_id);
        }
    });

    let CurrentMode(mode) = expect_context();
    let HistoryTimestamp(timestamp) = expect_context();
    let PlaybackActive(playing) = expect_context();
    let PlaybackSpeed(speed) = expect_context();
    let HistoryBoundsSignal(bounds) = expect_context();
    let HistoryFetchNonce(fetch_nonce) = expect_context();
    let HistoryLegacyGeometryActive(history_legacy_geometry_active) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let TerritoryGeometryStore(geo_store) = expect_context();
    let GuildColorStore(guild_color_store) = expect_context();

    // Event buffer
    let event_buffer: StoredValue<Vec<sequoia_shared::history::HistoryEvent>> =
        StoredValue::new(Vec::new());
    let buffer_end: StoredValue<i64> = StoredValue::new(0);
    let fetching: StoredValue<bool> = StoredValue::new(false);
    // Backoff: don't retry fetches until this timestamp (ms) passes
    let retry_after: StoredValue<f64> = StoredValue::new(0.0);
    // Fractional second accumulator — handles sub-second advancement at low speeds
    let frac_acc: StoredValue<f64> = StoredValue::new(0.0);
    // Track last known timestamp to detect user scrubbing (invalidates event buffer)
    let last_ts: StoredValue<i64> = StoredValue::new(0);
    // Sequence cursor for deterministic pagination across history events.
    let next_after_seq: StoredValue<u64> = StoredValue::new(0);
    // Season SR snapshots buffered from /api/history/sr-samples.
    let sr_buffer: StoredValue<Vec<HistorySrSnapshot>> = StoredValue::new(Vec::new());

    // Playback tick interval (100ms)
    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::<dyn Fn()>::new(move || {
        if mode.get_untracked() != MapMode::History || !playing.get_untracked() {
            return;
        }

        let spd = speed.get_untracked();
        let advance = spd * 0.1 + frac_acc.get_value();
        let advance_whole = advance.floor() as i64;
        frac_acc.set_value(advance - advance_whole as f64);

        if advance_whole > 0 {
            // Clamp to latest bound and auto-pause at end
            if let Some((_, latest)) = bounds.get_untracked() {
                let current = timestamp.get_untracked().unwrap_or(0);
                if current >= latest {
                    // At end of bounds — wrap to earliest and load snapshot
                    if let Some((earliest, _)) = bounds.get_untracked() {
                        timestamp.set(Some(earliest));
                        event_buffer.set_value(Vec::new());
                        sr_buffer.set_value(Vec::new());
                        buffer_end.set_value(0);
                        next_after_seq.set_value(0);
                        frac_acc.set_value(0.0);
                        crate::history::fetch_and_apply_with(
                            earliest,
                            crate::history::HistoryFetchContext {
                                mode,
                                history_fetch_nonce: fetch_nonce,
                                history_legacy_geometry_active,
                                history_scalar_sample,
                                history_sr_leaderboard,
                                geo_store,
                                guild_color_store,
                                territories,
                            },
                        );
                    }
                    return;
                }
                let new_ts = (current + advance_whole).min(latest);
                timestamp.set(Some(new_ts));
            } else {
                timestamp.update(|ts| {
                    if let Some(t) = ts {
                        *t += advance_whole;
                    }
                });
            }
        }

        let Some(current_ts) = timestamp.get_untracked() else {
            return;
        };

        // Detect user scrub: if timestamp jumped, invalidate event buffer
        let prev_ts = last_ts.get_value();
        if prev_ts != 0 && (current_ts - prev_ts).abs() > advance_whole.max(2) {
            event_buffer.set_value(Vec::new());
            sr_buffer.set_value(Vec::new());
            buffer_end.set_value(0);
            next_after_seq.set_value(0);
            frac_acc.set_value(0.0);
        }
        last_ts.set_value(current_ts);

        let mut samples = sr_buffer.get_value();
        let mut latest_sample_entries: Option<Vec<sequoia_shared::history::HistoryGuildSrEntry>> =
            None;
        let mut remaining_samples = Vec::new();
        for sample in samples.drain(..) {
            let sample_ts = parse_rfc3339_secs(&sample.sampled_at).unwrap_or(i64::MAX);
            if sample_ts <= current_ts {
                latest_sample_entries = Some(sample.entries);
            } else {
                remaining_samples.push(sample);
            }
        }
        if let Some(entries) = latest_sample_entries {
            history_sr_leaderboard.set(Some(entries));
        }
        sr_buffer.set_value(remaining_samples);

        // Process events that have been passed
        let mut events = event_buffer.get_value();
        let mut applied = Vec::new();
        let mut remaining = Vec::new();

        for event in events.drain(..) {
            let event_ts = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
                .map(|dt| dt.timestamp())
                .unwrap_or(0);

            if event_ts <= current_ts {
                applied.push(event);
            } else {
                remaining.push(event);
            }
        }

        // Apply passed events as territory changes
        if !applied.is_empty() {
            let geo = geo_store.get_value();
            let guild_colors = guild_color_store.get_value();
            let changes: Vec<sequoia_shared::TerritoryChange> = applied
                .iter()
                .filter_map(|e| {
                    let (location, resources, connections) = geo.get(&e.territory)?;

                    let previous_guild = match (&e.prev_guild_name, &e.prev_guild_prefix) {
                        (Some(name), Some(prefix)) => Some(sequoia_shared::GuildRef {
                            uuid: String::new(),
                            name: name.clone(),
                            prefix: prefix.clone(),
                            color: e
                                .prev_guild_color
                                .or_else(|| guild_colors.get(name).copied()),
                        }),
                        _ => None,
                    };

                    Some(sequoia_shared::TerritoryChange {
                        territory: e.territory.clone(),
                        guild: sequoia_shared::GuildRef {
                            uuid: e.guild_uuid.clone(),
                            name: e.guild_name.clone(),
                            prefix: e.guild_prefix.clone(),
                            color: e
                                .guild_color
                                .or_else(|| guild_colors.get(&e.guild_name).copied()),
                        },
                        previous_guild,
                        acquired: e.acquired_at.clone().unwrap_or_else(|| e.timestamp.clone()),
                        location: location.clone(),
                        resources: resources.clone(),
                        connections: connections.clone(),
                        runtime: None,
                    })
                })
                .collect();

            let now = js_sys::Date::now();
            let duration_ms = playback_animation_duration(spd);
            territories.update(|map| {
                apply_changes(map, &changes, now, duration_ms);
            });
        }

        event_buffer.set_value(remaining);

        // Pre-fetch more events when buffer is running low (with backoff on failure)
        let now_ms = js_sys::Date::now();
        let buf_len = event_buffer.get_value().len();
        let buf_end = buffer_end.get_value();
        if buf_len < 50
            && !fetching.get_value()
            && now_ms >= retry_after.get_value()
            && (buf_end == 0 || current_ts + 1800 > buf_end)
        {
            fetching.set_value(true);
            let fetch_from = if buf_end > current_ts {
                buf_end
            } else {
                current_ts
            };
            let to = fetch_from + 3600;
            spawn_local(async move {
                let mut cursor_time = fetch_from;
                let mut cursor_seq = next_after_seq.get_value();
                let mut all_events = Vec::new();
                let mut errored = false;

                // Paginate: up to 10 pages (5000 events max per prefetch cycle)
                for _ in 0..10 {
                    match fetch_events(cursor_time, to, Some(cursor_seq), 500).await {
                        Ok(result) => {
                            let has_more = result.has_more;
                            let page = result.events;

                            if page.is_empty() {
                                break;
                            }

                            // Primary cursor: stream sequence (deterministic, gap-safe).
                            // Fallback: timestamp (legacy server compatibility).
                            if has_more {
                                let max_seq = page.iter().map(|e| e.stream_seq).max().unwrap_or(0);
                                if max_seq > cursor_seq {
                                    cursor_seq = max_seq;
                                } else if let Some(last) = page.last() {
                                    if let Ok(dt) =
                                        chrono::DateTime::parse_from_rfc3339(&last.timestamp)
                                    {
                                        cursor_time = dt.timestamp();
                                    } else {
                                        all_events.extend(page);
                                        break;
                                    }
                                }
                            }

                            all_events.extend(page);

                            if !has_more {
                                break;
                            }
                        }
                        Err(e) => {
                            web_sys::console::warn_1(&format!("Playback fetch error: {e}").into());
                            retry_after.set_value(js_sys::Date::now() + 5000.0);
                            errored = true;
                            break;
                        }
                    }
                }

                if !errored {
                    let mut sr_fetch_failed = false;
                    let all_sr_samples = match fetch_sr_samples(fetch_from, to).await {
                        Ok(samples) => samples,
                        Err(e) => {
                            web_sys::console::warn_1(
                                &format!("Playback SR sample fetch error: {e}").into(),
                            );
                            retry_after.set_value(js_sys::Date::now() + 5000.0);
                            sr_fetch_failed = true;
                            Vec::new()
                        }
                    };

                    let mut buf = event_buffer.get_value();
                    buf.extend(all_events);
                    event_buffer.set_value(buf);

                    let mut sr_buf = sr_buffer.get_value();
                    sr_buf.extend(all_sr_samples);
                    sr_buf.sort_by(|a, b| {
                        parse_rfc3339_secs(&a.sampled_at)
                            .unwrap_or(i64::MAX)
                            .cmp(&parse_rfc3339_secs(&b.sampled_at).unwrap_or(i64::MAX))
                    });
                    sr_buf.dedup_by(|a, b| a.sampled_at == b.sampled_at);
                    sr_buffer.set_value(sr_buf);

                    if !sr_fetch_failed {
                        buffer_end.set_value(to);
                    }
                    next_after_seq.set_value(cursor_seq);
                }
                fetching.set_value(false);
            });
        }
    });

    let Ok(interval_id) = window
        .set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 100)
    else {
        return;
    };
    PLAYBACK_INTERVAL_BINDING.with(|slot| {
        *slot.borrow_mut() = Some(PlaybackIntervalBinding {
            window: window.clone(),
            interval_id,
            _callback: cb,
        });
    });
}
