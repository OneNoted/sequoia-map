use std::cell::RefCell;
use std::collections::VecDeque;

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sequoia_shared::history::{HistoryEvent, HistoryEvents, HistoryGuildSrEntry, HistorySrSamples};
use sequoia_shared::{GuildRef, TerritoryChange};

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBoundsSignal, HistoryFetchNonce,
    HistoryLegacyGeometryActive, HistorySeasonLeaderboard, HistorySeasonScalarSample,
    HistoryTimestamp, MapMode, PlaybackActive, PlaybackSpeed, TerritoryGeometryStore,
};
use crate::territory::{ClientTerritoryMap, apply_changes};

const TICK_MS: i32 = 100;
const PAGE_LIMIT: u32 = 1000;
/// Events held ahead of the clock at most; fetching pauses until playback consumes them.
const MAX_BUFFERED_EVENTS: usize = 5_000;
const MICROS: i64 = 1_000_000;

struct PlaybackIntervalBinding {
    window: web_sys::Window,
    interval_id: i32,
    _callback: Closure<dyn Fn()>,
}

/// One upcoming ownership change, decoded once when its page arrives.
struct Pending {
    at_micros: i64,
    territory: String,
    guild: GuildRef,
    previous: Option<GuildRef>,
    acquired: String,
}

/// Events after the clock that playback will apply, and how far that knowledge reaches.
///
/// `covered_micros` is the instant up to which every recorded event is either applied or in
/// `events`; the clock never moves past it, so a slow or partial fetch pauses playback
/// instead of skipping changes. Everything here belongs to one `epoch`: a seek, an exit or a
/// new visit starts another, and replies of an older epoch are dropped.
struct Buffer {
    epoch: u64,
    /// The seek this epoch starts from; nothing is applied until its state is on the map.
    seek_nonce: u64,
    ready: bool,
    events: VecDeque<Pending>,
    covered_micros: i64,
    /// The window being paged, with the last event received, when a page said there was more.
    window: Option<(i64, i64)>,
    cursor: Option<(u64, u16)>,
    fetch: Option<web_sys::AbortController>,
    retry_after_ms: f64,
    samples: VecDeque<(i64, Vec<HistoryGuildSrEntry>)>,
    samples_covered_micros: i64,
    samples_fetch: Option<web_sys::AbortController>,
    fractional_secs: f64,
}

impl Buffer {
    const fn new() -> Self {
        Self {
            epoch: 0,
            seek_nonce: 0,
            ready: false,
            events: VecDeque::new(),
            covered_micros: i64::MIN,
            window: None,
            cursor: None,
            fetch: None,
            retry_after_ms: 0.0,
            samples: VecDeque::new(),
            samples_covered_micros: i64::MIN,
            samples_fetch: None,
            fractional_secs: 0.0,
        }
    }

    fn restart(&mut self, at_secs: i64, seek_nonce: u64) {
        for controller in [self.fetch.take(), self.samples_fetch.take()]
            .into_iter()
            .flatten()
        {
            controller.abort();
        }
        let epoch = self.epoch.wrapping_add(1);
        *self = Self::new();
        self.epoch = epoch;
        self.seek_nonce = seek_nonce;
        self.covered_micros = at_secs.saturating_mul(MICROS);
        self.samples_covered_micros = self.covered_micros;
    }
}

thread_local! {
    static PLAYBACK_INTERVAL_BINDING: RefCell<Option<PlaybackIntervalBinding>> = const { RefCell::new(None) };
    static BUFFER: RefCell<Buffer> = const { RefCell::new(Buffer::new()) };
}

/// A seek to `at_secs` is in flight: forget buffered events and wait for its state.
pub fn restart_at(at_secs: i64, seek_nonce: u64) {
    BUFFER.with(|buffer| buffer.borrow_mut().restart(at_secs, seek_nonce));
}

/// The state of seek `seek_nonce` is on the map; playback may continue from it.
pub fn seek_applied(seek_nonce: u64) {
    BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        if buffer.seek_nonce == seek_nonce {
            buffer.ready = true;
        }
    });
}

/// Leaving history: drop buffered events and abort their requests.
pub fn stop() {
    BUFFER.with(|buffer| buffer.borrow_mut().restart(0, 0));
}

fn rfc3339_secs(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn rfc3339_micros(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|at| at.timestamp_micros())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<T, String> {
    let response = gloo_net::http::Request::get(url)
        .abort_signal(signal)
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    response
        .json::<T>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

fn pending_from(event: HistoryEvent) -> Option<Pending> {
    let at_micros = rfc3339_micros(&event.timestamp)?;
    let previous = match (event.prev_guild_name, event.prev_guild_prefix) {
        (Some(name), Some(prefix)) => Some(GuildRef {
            uuid: String::new(),
            name,
            prefix,
            color: event.prev_guild_color,
        }),
        _ => None,
    };
    Some(Pending {
        at_micros,
        acquired: event.acquired_at.unwrap_or(event.timestamp),
        territory: event.territory,
        guild: GuildRef {
            uuid: event.guild_uuid,
            name: event.guild_name,
            prefix: event.guild_prefix,
            color: event.guild_color,
        },
        previous,
    })
}

/// Request the next page of events after the covered instant, unless one is in flight,
/// the buffer is full or the last attempt failed recently.
fn request_events(span_secs: i64, latest_secs: i64) {
    let request = BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        if !buffer.ready
            || buffer.fetch.is_some()
            || buffer.events.len() >= MAX_BUFFERED_EVENTS
            || js_sys::Date::now() < buffer.retry_after_ms
            || buffer.covered_micros >= latest_secs.saturating_mul(MICROS)
        {
            return None;
        }
        let (from, to) = buffer.window.unwrap_or_else(|| {
            let from = buffer.covered_micros.div_euclid(MICROS);
            (from, from.saturating_add(span_secs).min(latest_secs))
        });
        let mut url = format!(
            "/api/history/events?from={}&to={}&limit={PAGE_LIMIT}",
            rfc3339_secs(from)?,
            rfc3339_secs(to)?,
        );
        if let Some((seq, ordinal)) = buffer.cursor {
            url.push_str(&format!("&after_seq={seq}&after_ordinal={ordinal}"));
        }
        let controller = web_sys::AbortController::new().ok()?;
        let signal = controller.signal();
        buffer.fetch = Some(controller);
        Some((buffer.epoch, from, to, url, signal))
    });
    let Some((epoch, from, to, url, signal)) = request else {
        return;
    };
    spawn_local(async move {
        let page = get_json::<HistoryEvents>(&url, Some(&signal)).await;
        BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            if buffer.epoch != epoch {
                return;
            }
            buffer.fetch = None;
            let page = match page {
                Ok(page) => page,
                Err(error) => {
                    web_sys::console::warn_1(&format!("Playback fetch error: {error}").into());
                    buffer.retry_after_ms = js_sys::Date::now() + 5000.0;
                    return;
                }
            };
            let last = page.events.last().map(|event| {
                (
                    event.stream_seq,
                    event.ordinal,
                    rfc3339_micros(&event.timestamp),
                )
            });
            buffer
                .events
                .extend(page.events.into_iter().filter_map(pending_from));
            match (page.has_more, last) {
                (true, Some((seq, ordinal, Some(last_micros)))) => {
                    // Later pages hold events recorded at or after the last one received.
                    buffer.covered_micros = buffer.covered_micros.max(last_micros - 1);
                    buffer.window = Some((from, to));
                    buffer.cursor = Some((seq, ordinal));
                }
                _ => {
                    buffer.covered_micros = to.saturating_mul(MICROS);
                    buffer.window = None;
                    buffer.cursor = None;
                }
            }
        });
    });
}

/// Season rating samples ahead of the clock; they never hold the clock back.
fn request_samples(span_secs: i64, latest_secs: i64) {
    let request = BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        if !buffer.ready
            || buffer.samples_fetch.is_some()
            || buffer.samples.len() > 64
            || js_sys::Date::now() < buffer.retry_after_ms
            || buffer.samples_covered_micros >= latest_secs.saturating_mul(MICROS)
        {
            return None;
        }
        let from = buffer.samples_covered_micros.div_euclid(MICROS);
        let to = from.saturating_add(span_secs).min(latest_secs);
        let url = format!(
            "/api/history/sr-samples?from={}&to={}",
            rfc3339_secs(from)?,
            rfc3339_secs(to)?
        );
        let controller = web_sys::AbortController::new().ok()?;
        let signal = controller.signal();
        buffer.samples_fetch = Some(controller);
        Some((buffer.epoch, to, url, signal))
    });
    let Some((epoch, to, url, signal)) = request else {
        return;
    };
    spawn_local(async move {
        let samples = get_json::<HistorySrSamples>(&url, Some(&signal)).await;
        BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            if buffer.epoch != epoch {
                return;
            }
            buffer.samples_fetch = None;
            match samples {
                Ok(payload) => {
                    buffer
                        .samples
                        .extend(payload.samples.into_iter().filter_map(|sample| {
                            Some((rfc3339_micros(&sample.sampled_at)?, sample.entries))
                        }));
                    buffer.samples_covered_micros = to.saturating_mul(MICROS);
                }
                Err(error) => {
                    web_sys::console::warn_1(
                        &format!("Playback SR sample fetch error: {error}").into(),
                    );
                    buffer.retry_after_ms = js_sys::Date::now() + 5000.0;
                }
            }
        });
    });
}

/// Remove and return, in stream order, every buffered event recorded by `until`. Stream
/// order and recorded time can disagree in converted history, so any buffered event may be
/// due; a seek applies the latest stream position per territory, so an event that becomes
/// due also supersedes earlier-positioned events for its territory still waiting.
fn take_due(events: &mut VecDeque<Pending>, until: i64) -> Vec<Pending> {
    if !events.iter().any(|event| event.at_micros <= until) {
        return Vec::new();
    }
    let mut due = Vec::new();
    let mut waiting = VecDeque::with_capacity(events.len());
    for event in events.drain(..) {
        if event.at_micros > until {
            waiting.push_back(event);
            continue;
        }
        if !waiting.is_empty() {
            waiting.retain(|earlier: &Pending| earlier.territory != event.territory);
        }
        due.push(event);
    }
    *events = waiting;
    due
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
/// The interval runs every 100ms but only advances time when in history mode and playing.
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
    let fetch_ctx = crate::history::HistoryFetchContext {
        mode,
        history_fetch_nonce: fetch_nonce,
        history_legacy_geometry_active,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
    };

    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::<dyn Fn()>::new(move || {
        if mode.get_untracked() != MapMode::History || !playing.get_untracked() {
            return;
        }
        let (Some((earliest, latest)), Some(current)) =
            (bounds.get_untracked(), timestamp.get_untracked())
        else {
            return;
        };
        if current >= latest {
            // At the end of the retained range: start over from the beginning.
            timestamp.set(Some(earliest));
            crate::history::fetch_and_apply_with(earliest, fetch_ctx);
            return;
        }

        let speed_now = speed.get_untracked();
        // Look further ahead at higher speeds so fetching stays ahead of the clock.
        let span_secs = (speed_now * 600.0).clamp(3600.0, 6.0 * 3600.0) as i64;
        request_events(span_secs, latest);
        request_samples(span_secs, latest);

        let (next, due, sample) = BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            if !buffer.ready {
                return (current, Vec::new(), None);
            }
            let advance = speed_now * f64::from(TICK_MS) / 1000.0 + buffer.fractional_secs;
            let whole = advance.floor();
            buffer.fractional_secs = advance - whole;
            let covered_secs = buffer.covered_micros.div_euclid(MICROS);
            let next = (current + whole as i64)
                .min(latest)
                .min(covered_secs.max(current));
            let until = next.saturating_mul(MICROS);
            let due = take_due(&mut buffer.events, until);
            let mut sample = None;
            while buffer
                .samples
                .front()
                .is_some_and(|(at_micros, _)| *at_micros <= until)
            {
                sample = buffer.samples.pop_front().map(|(_, entries)| entries);
            }
            (next, due, sample)
        });

        if next != current {
            timestamp.set(Some(next));
        }
        if let Some(entries) = sample {
            history_sr_leaderboard.set(Some(entries));
        }
        if due.is_empty() {
            return;
        }
        let changes: Vec<TerritoryChange> = geo_store.with_value(|geo| {
            guild_color_store.with_value(|guild_colors| {
                due.into_iter()
                    .filter_map(|event| {
                        let (location, resources, connections) = geo.get(&event.territory)?;
                        let color_of = |guild: GuildRef| GuildRef {
                            color: guild
                                .color
                                .or_else(|| guild_colors.get(&guild.name).copied()),
                            ..guild
                        };
                        Some(TerritoryChange {
                            territory: event.territory,
                            guild: color_of(event.guild),
                            previous_guild: event.previous.map(color_of),
                            acquired: event.acquired,
                            location: location.clone(),
                            resources: resources.clone(),
                            connections: connections.clone(),
                            runtime: None,
                        })
                    })
                    .collect()
            })
        });
        if changes.is_empty() {
            return;
        }
        let now = js_sys::Date::now();
        let duration_ms = playback_animation_duration(speed_now);
        territories.update(|map| apply_changes(map, &changes, now, duration_ms));
    });

    let Ok(interval_id) = window.set_interval_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        TICK_MS,
    ) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(territory: &str, at_micros: i64, guild: &str) -> Pending {
        Pending {
            at_micros,
            territory: territory.to_string(),
            guild: GuildRef {
                uuid: guild.to_string(),
                name: guild.to_string(),
                prefix: guild.to_string(),
                color: None,
            },
            previous: None,
            acquired: String::new(),
        }
    }

    fn applied(events: &[Pending]) -> Vec<(&str, &str)> {
        events
            .iter()
            .map(|event| (event.territory.as_str(), event.guild.name.as_str()))
            .collect()
    }

    #[test]
    fn events_recorded_out_of_stream_order_apply_when_a_seek_would_show_them() {
        // Two territories in stream order recorded at 20 and 10: each applies at its time.
        let mut pair = VecDeque::from([event("Alpha", 20, "First"), event("Beta", 10, "Second")]);
        assert_eq!(applied(&take_due(&mut pair, 10)), vec![("Beta", "Second")]);
        assert_eq!(applied(&take_due(&mut pair, 20)), vec![("Alpha", "First")]);

        // Stream order: Alpha recorded at 20, then Beta at 10, then Alpha again at 15.
        let mut buffer = VecDeque::from([
            event("Alpha", 20, "First"),
            event("Beta", 10, "Second"),
            event("Alpha", 15, "Third"),
        ]);

        // At 10 a seek shows Beta's change only.
        assert_eq!(
            applied(&take_due(&mut buffer, 10)),
            vec![("Beta", "Second")]
        );
        // At 15 the later stream position for Alpha applies and supersedes the one at 20.
        assert_eq!(
            applied(&take_due(&mut buffer, 15)),
            vec![("Alpha", "Third")]
        );
        // At 20 nothing else applies: a seek to 20 shows Third for Alpha, not First.
        assert!(take_due(&mut buffer, 20).is_empty());
        assert!(buffer.is_empty());
    }
}
