use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sequoia_shared::history::{
    HistoryEvent, HistoryEvents, HistoryGuildSrEntry, HistorySrSamples, HistoryStateChange,
    HistoryStateEvent, HistoryTerritoryState,
};
use sequoia_shared::{GuildRef, Territory, TerritoryChange, TerritoryState};

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBoundsSignal, HistoryFetchNonce,
    HistoryLegacyGeometryActive, HistoryMapTime, HistorySeasonLeaderboard,
    HistorySeasonScalarSample, HistoryTimestamp, MapMode, PlaybackActive, PlaybackSpeed,
    TerritoryGeometryStore,
};
use crate::history::historical_parts;
use crate::territory::{ClientTerritory, ClientTerritoryMap, apply_changes};

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
    /// Stream position and order within it.
    position: (u64, u16),
    territory: String,
    guild: GuildRef,
    previous: Option<GuildRef>,
    acquired: String,
}

/// Official-API state changes at one stream position, decoded once when their page arrives.
struct PendingState {
    at_micros: i64,
    observed_at: String,
    changes: Vec<HistoryStateChange>,
}

impl PendingState {
    fn from_event(event: HistoryStateEvent) -> Option<Self> {
        Some(Self {
            at_micros: rfc3339_micros(&event.timestamp)?,
            observed_at: event.observed_at,
            changes: event.changes,
        })
    }
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
    /// State changes after the clock, in stream order; their recorded times follow it.
    states: VecDeque<PendingState>,
    /// Each territory's recorded state at the clock: the seek's, with due changes applied.
    territory_states: BTreeMap<String, HistoryTerritoryState>,
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
    /// The latest stream position applied per territory in this epoch: an older one that
    /// arrives in a later page is superseded, as in a seek.
    applied: BTreeMap<String, (u64, u16)>,
}

impl Buffer {
    const fn new() -> Self {
        Self {
            epoch: 0,
            seek_nonce: 0,
            ready: false,
            events: VecDeque::new(),
            states: VecDeque::new(),
            territory_states: BTreeMap::new(),
            covered_micros: i64::MIN,
            window: None,
            cursor: None,
            fetch: None,
            retry_after_ms: 0.0,
            samples: VecDeque::new(),
            samples_covered_micros: i64::MIN,
            samples_fetch: None,
            fractional_secs: 0.0,
            applied: BTreeMap::new(),
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

/// The state of seek `seek_nonce`, with its recorded territory `states`, is on the map;
/// playback may continue from it.
pub fn seek_applied(seek_nonce: u64, states: HashMap<String, HistoryTerritoryState>) {
    BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        if buffer.seek_nonce == seek_nonce {
            buffer.ready = true;
            buffer.territory_states = states.into_iter().collect();
        }
    });
}

/// Apply every buffered state change recorded by `until` and return the territories whose
/// recorded state changed.
fn take_due_states(
    states: &mut VecDeque<PendingState>,
    territory_states: &mut BTreeMap<String, HistoryTerritoryState>,
    until: i64,
) -> Vec<String> {
    let mut touched = Vec::new();
    while states
        .front()
        .is_some_and(|pending| pending.at_micros <= until)
    {
        let Some(pending) = states.pop_front() else {
            break;
        };
        for change in pending.changes {
            let recorded = territory_states
                .entry(change.territory.clone())
                .or_insert_with(|| HistoryTerritoryState {
                    observed_at: pending.observed_at.clone(),
                    state: TerritoryState::default(),
                });
            recorded.observed_at.clone_from(&pending.observed_at);
            for field in change.fields {
                recorded.state.apply(field);
            }
            touched.push(change.territory);
        }
    }
    touched.sort_unstable();
    touched.dedup();
    touched
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
        position: (event.stream_seq, event.ordinal),
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
            || buffer.events.len() + buffer.states.len() >= MAX_BUFFERED_EVENTS
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
            let last = page
                .events
                .iter()
                .map(|event| (event.stream_seq, event.ordinal, event.timestamp.as_str()))
                .chain(
                    page.states
                        .iter()
                        .map(|state| (state.stream_seq, 0, state.timestamp.as_str())),
                )
                .max_by_key(|&(seq, ordinal, _)| (seq, ordinal))
                .map(|(seq, ordinal, timestamp)| (seq, ordinal, rfc3339_micros(timestamp)));
            buffer
                .events
                .extend(page.events.into_iter().filter_map(pending_from));
            buffer
                .states
                .extend(page.states.into_iter().filter_map(PendingState::from_event));
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

/// Remove and return, in stream order, every buffered event recorded by `until` that a seek
/// would show. Stream order and recorded time can disagree in converted history, so any
/// buffered event may be due; a seek applies the latest stream position per territory, so an
/// event that becomes due supersedes earlier positions for its territory, whether still
/// waiting here or arriving in a later page (`applied` remembers the positions applied).
fn take_due(
    events: &mut VecDeque<Pending>,
    applied: &mut BTreeMap<String, (u64, u16)>,
    until: i64,
) -> Vec<Pending> {
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
        match applied.get_mut(&event.territory) {
            Some(position) if *position >= event.position => continue,
            Some(position) => *position = event.position,
            None => {
                applied.insert(event.territory.clone(), event.position);
            }
        }
        due.push(event);
    }
    *events = waiting;
    due
}

/// Show each territory's newly recorded state. A change of runtime alone updates it in
/// place, as the live feed does; a change of where it is, what it produces or its links
/// rebuilds the territory, as a seek would.
fn apply_recorded_states(
    map: &mut ClientTerritoryMap,
    geometry: &crate::app::TerritoryGeometryMap,
    touched: Vec<(HistoryTerritoryState, String)>,
) {
    for (recorded, name) in touched {
        let Some(current) = map.get_mut(&name) else {
            continue;
        };
        let territory = &current.territory;
        let holding = (
            territory.guild.uuid.as_str(),
            territory.acquired.timestamp_micros(),
        );
        let Some((location, resources, connections, runtime)) =
            historical_parts(geometry.get(&name), Some(&recorded), holding)
        else {
            continue;
        };
        if territory.location == location
            && territory.resources == resources
            && territory.connections == connections
        {
            current.territory.runtime = runtime;
        } else {
            *current = ClientTerritory::from_territory(
                &name,
                Territory {
                    guild: territory.guild.clone(),
                    acquired: territory.acquired,
                    location,
                    resources,
                    connections,
                    runtime,
                },
            );
        }
    }
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
    let HistoryMapTime(history_map_time) = expect_context();
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
        history_map_time,
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

        let (next, due, touched, sample) = BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            if !buffer.ready {
                return (current, Vec::new(), Vec::new(), None);
            }
            let advance = speed_now * f64::from(TICK_MS) / 1000.0 + buffer.fractional_secs;
            let whole = advance.floor();
            buffer.fractional_secs = advance - whole;
            let covered_secs = buffer.covered_micros.div_euclid(MICROS);
            let next = (current + whole as i64)
                .min(latest)
                .min(covered_secs.max(current));
            let until = next.saturating_mul(MICROS);
            let buffer = &mut *buffer;
            let due = take_due(&mut buffer.events, &mut buffer.applied, until);
            let touched = take_due_states(&mut buffer.states, &mut buffer.territory_states, until);
            let recorded = |name: &str| buffer.territory_states.get(name).cloned();
            let due: Vec<_> = due
                .into_iter()
                .map(|event| {
                    let state = recorded(&event.territory);
                    (event, state)
                })
                .collect();
            let touched: Vec<_> = touched
                .into_iter()
                .filter_map(|name| Some((recorded(&name)?, name)))
                .collect();
            let mut sample = None;
            while buffer
                .samples
                .front()
                .is_some_and(|(at_micros, _)| *at_micros <= until)
            {
                sample = buffer.samples.pop_front().map(|(_, entries)| entries);
            }
            (next, due, touched, sample)
        });

        if next != current {
            timestamp.set(Some(next));
            history_map_time.set(Some(next));
        }
        if let Some(entries) = sample {
            history_sr_leaderboard.set(Some(entries));
        }
        if !touched.is_empty() {
            geo_store.with_value(|geo| {
                territories.update(|map| apply_recorded_states(map, geo, touched));
            });
        }
        if due.is_empty() {
            return;
        }
        let changes: Vec<TerritoryChange> = geo_store.with_value(|geo| {
            guild_color_store.with_value(|guild_colors| {
                due.into_iter()
                    .filter_map(|(event, recorded)| {
                        let acquired = rfc3339_micros(&event.acquired).unwrap_or(event.at_micros);
                        let (location, resources, connections, runtime) = historical_parts(
                            geo.get(&event.territory),
                            recorded.as_ref(),
                            (&event.guild.uuid, acquired),
                        )?;
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
                            location,
                            resources,
                            connections,
                            runtime,
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
    use sequoia_shared::TerritoryStateField;

    use super::*;

    fn event(territory: &str, seq: u64, at_micros: i64, guild: &str) -> Pending {
        Pending {
            at_micros,
            position: (seq, 0),
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
    fn playback_shows_a_recorded_state_only_for_its_holding() {
        let holding = |uuid: &str| Territory {
            guild: GuildRef {
                uuid: uuid.to_string(),
                name: uuid.to_string(),
                prefix: uuid.to_string(),
                color: None,
            },
            acquired: "2026-01-01T00:00:00Z".parse().expect("acquired"),
            location: sequoia_shared::Region {
                start: [0, 0],
                end: [10, 10],
            },
            resources: sequoia_shared::Resources::default(),
            connections: Vec::new(),
            runtime: None,
        };
        let rates = |emeralds, ore| sequoia_shared::Resources {
            emeralds,
            ore,
            ..sequoia_shared::Resources::default()
        };
        let recorded = HistoryTerritoryState {
            observed_at: "2026-11-02T11:17:44+00:00".to_string(),
            state: TerritoryState {
                owner: Some(sequoia_shared::StateOwner {
                    guild_uuid: "old".to_string(),
                    acquired_micros: 1_767_225_600_000_000,
                }),
                headquarters: Some(true),
                generation: Some(rates(9000, 0)),
                base_generation: Some(rates(9000, 3600)),
                stored: Some(rates(500, 0)),
                ..TerritoryState::default()
            },
        };
        for (uuid, shows_runtime) in [("old", true), ("new", false)] {
            let mut map = ClientTerritoryMap::from([(
                "A".to_string(),
                ClientTerritory::from_territory("A", holding(uuid)),
            )]);
            apply_recorded_states(
                &mut map,
                &crate::app::TerritoryGeometryMap::from([(
                    "A".to_string(),
                    (
                        holding(uuid).location,
                        sequoia_shared::Resources::default(),
                        Vec::new(),
                    ),
                )]),
                vec![(recorded.clone(), "A".to_string())],
            );
            let territory = &map["A"].territory;
            assert_eq!(
                territory.resources,
                rates(9000, 0),
                "{uuid}: ore is not produced"
            );
            assert_eq!(territory.runtime.is_some(), shows_runtime, "{uuid}");
        }
    }

    #[test]
    fn due_state_changes_build_on_the_seek_state_in_order() {
        let change = |at_micros, territory: &str, fields| PendingState {
            at_micros,
            observed_at: format!("observed {at_micros}"),
            changes: vec![HistoryStateChange {
                territory: territory.to_string(),
                fields,
            }],
        };
        let mut territory_states = BTreeMap::from([(
            "A".to_string(),
            HistoryTerritoryState {
                observed_at: "seek".to_string(),
                state: TerritoryState {
                    headquarters: Some(false),
                    treasury: Some("LOW".to_string()),
                    ..TerritoryState::default()
                },
            },
        )]);
        let mut states = VecDeque::from([
            change(10, "A", vec![TerritoryStateField::Headquarters(Some(true))]),
            change(
                20,
                "B",
                vec![TerritoryStateField::Treasury(Some("MEDIUM".to_string()))],
            ),
            change(30, "A", vec![TerritoryStateField::Treasury(None)]),
        ]);

        assert_eq!(
            take_due_states(&mut states, &mut territory_states, 20),
            vec!["A", "B"]
        );
        assert_eq!(territory_states["A"].state.headquarters, Some(true));
        assert_eq!(territory_states["A"].state.treasury.as_deref(), Some("LOW"));
        assert_eq!(territory_states["A"].observed_at, "observed 10");
        assert_eq!(
            territory_states["B"].state.treasury.as_deref(),
            Some("MEDIUM")
        );

        assert_eq!(
            take_due_states(&mut states, &mut territory_states, 30),
            vec!["A"]
        );
        assert_eq!(
            territory_states["A"].state.treasury, None,
            "a field can become unknown"
        );
        assert!(states.is_empty());
    }

    #[test]
    fn events_recorded_out_of_stream_order_apply_when_a_seek_would_show_them() {
        // Two territories in stream order recorded at 20 and 10: each applies at its time.
        let mut seen = BTreeMap::new();
        let mut pair = VecDeque::from([
            event("Alpha", 1, 20, "First"),
            event("Beta", 2, 10, "Second"),
        ]);
        assert_eq!(
            applied(&take_due(&mut pair, &mut seen, 10)),
            vec![("Beta", "Second")]
        );
        assert_eq!(
            applied(&take_due(&mut pair, &mut seen, 20)),
            vec![("Alpha", "First")]
        );

        // Alpha recorded at 20, Beta at 10, Alpha again at 15, in that stream order.
        let mut seen = BTreeMap::new();
        let mut buffer = VecDeque::from([
            event("Alpha", 1, 20, "First"),
            event("Beta", 2, 10, "Second"),
            event("Alpha", 3, 15, "Third"),
        ]);
        assert_eq!(
            applied(&take_due(&mut buffer, &mut seen, 10)),
            vec![("Beta", "Second")]
        );
        // At 15 the later stream position for Alpha applies and supersedes the one at 20.
        assert_eq!(
            applied(&take_due(&mut buffer, &mut seen, 15)),
            vec![("Alpha", "Third")]
        );
        // At 20 nothing else applies: a seek to 20 shows Third for Alpha, not First.
        assert!(take_due(&mut buffer, &mut seen, 20).is_empty());
        assert!(buffer.is_empty());
    }

    #[test]
    fn an_older_position_arriving_in_a_later_page_does_not_override_a_newer_one() {
        // Alpha's position 2 was recorded at 3590 and comes in the page for (0, 3600];
        // position 1 was recorded at 3610 and comes in the next page, (3600, 7200].
        let mut seen = BTreeMap::new();
        let mut buffer = VecDeque::from([event("Alpha", 2, 3_590, "Second")]);
        assert_eq!(
            applied(&take_due(&mut buffer, &mut seen, 3_600)),
            vec![("Alpha", "Second")]
        );
        buffer.push_back(event("Alpha", 1, 3_610, "First"));
        // A seek to 3620 shows Second, the later stream position.
        assert!(take_due(&mut buffer, &mut seen, 3_620).is_empty());
        assert!(buffer.is_empty());
    }
}
