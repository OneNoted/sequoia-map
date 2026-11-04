use leptos::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBarHeight, HistoryBoundsSignal, HistoryBufferModeActive,
    HistoryBufferedUpdates, HistoryFetchNonce, HistoryLegacyGeometryActive, HistoryMapTime,
    HistorySeasonLeaderboard, HistorySeasonScalarSample, HistoryTimestamp, IsMobile, LastLiveSeq,
    LiveHandoffResyncCount, MapMode, NeedsLiveResync, PlaybackActive, PlaybackSpeed, SidebarOpen,
    SidebarWidth, TerritoryGeometryStore,
};
use crate::history;
use crate::precision_scrub::{self, FINEST_LEVEL, Rail, Scrub};
use crate::territory::ClientTerritoryMap;

const HOUR_SECS: i64 = 60 * 60;
const DAY_SECS: i64 = 24 * HOUR_SECS;
const HISTORY_FOCUS_SPANS: &[(Option<i64>, &str)] = &[
    (Some(HOUR_SECS), "1h"),
    (Some(6 * HOUR_SECS), "6h"),
    (Some(DAY_SECS), "24h"),
    (Some(7 * DAY_SECS), "7d"),
    (Some(30 * DAY_SECS), "30d"),
    (None, "All"),
];
const SPEEDS: [f64; 4] = [1.0, 10.0, 60.0, 360.0];

const PLAY_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M4 2.5v11l9.5-5.5z"/></svg>"#;
const PAUSE_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><rect x="3.5" y="2.5" width="3" height="11" rx="0.5"/><rect x="9.5" y="2.5" width="3" height="11" rx="0.5"/></svg>"#;
const STEP_BACK_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><rect x="3" y="3" width="2" height="10" rx="0.5"/><path d="M13 3v10L6 8z"/></svg>"#;
const STEP_FORWARD_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><rect x="11" y="3" width="2" height="10" rx="0.5"/><path d="M3 3v10l7-5z"/></svg>"#;
const MORE_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><circle cx="3.5" cy="8" r="1.5"/><circle cx="8" cy="8" r="1.5"/><circle cx="12.5" cy="8" r="1.5"/></svg>"#;
const LIVE_SVG: &str = r#"<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"/></svg>"#;
const LIFT_SVG: &str = r#"<svg viewBox="0 0 10 10" fill="currentColor" aria-hidden="true"><path d="M5 1.5L9 7H1z"/></svg>"#;
const DEFAULT_HISTORY_FOCUS_SPAN: Option<i64> = Some(6 * HOUR_SECS);

/// How far beside a rail's thumb a press still takes hold of it rather than jumping; a
/// fingertip is wider than a cursor.
const THUMB_SLACK_MOUSE_PX: f64 = 3.0;
const THUMB_SLACK_TOUCH_PX: f64 = 10.0;
/// The precision cue sits this far above the pointer, clear of a fingertip, and never lower
/// than just above the bar or higher than just under the site navigation.
const CUE_GAP_MOUSE_PX: f64 = 22.0;
const CUE_GAP_TOUCH_PX: f64 = 56.0;
const CUE_ABOVE_BAR_PX: f64 = 10.0;
const CUE_HIGHEST_PX: f64 = 100.0;
/// About half the cue's width: how close to the bar's ends its centre may come.
const CUE_HALF_WIDTH_PX: f64 = 112.0;

#[wasm_bindgen]
extern "C" {
    /// A `MouseEvent` read through getters that keep the doubles browsers report; web-sys's
    /// stable `client_x`/`client_y` return `i32`.
    #[wasm_bindgen(extends = web_sys::MouseEvent)]
    type FractionalMouseEvent;
    #[wasm_bindgen(method, getter = clientX)]
    fn client_x(this: &FractionalMouseEvent) -> f64;
    #[wasm_bindgen(method, getter = clientY)]
    fn client_y(this: &FractionalMouseEvent) -> f64;
}

fn client_position(event: &web_sys::PointerEvent) -> (f64, f64) {
    let precise = event.unchecked_ref::<FractionalMouseEvent>();
    (precise.client_x(), precise.client_y())
}

/// `input` as laid out now. Its thumb's diameter is `--tl-thumb` (client/input.css).
fn rail_of(input: &web_sys::HtmlInputElement) -> Option<Rail> {
    let thumb = web_sys::window()?
        .get_computed_style(input)
        .ok()??
        .get_property_value("--tl-thumb")
        .ok()?
        .trim()
        .strip_suffix("px")?
        .parse()
        .ok()?;
    let rect = input.get_bounding_client_rect();
    let rail = Rail {
        left: rect.left(),
        width: rect.width(),
        thumb,
        center_y: rect.top() + rect.height() / 2.0,
        min: input.min().parse().ok()?,
        max: input.max().parse().ok()?,
    };
    (rail.max > rail.min && rail.width > rail.thumb).then_some(rail)
}

/// A pointer holding one of the rails.
struct HeldRail {
    scrub: Scrub,
    overview: bool,
    input: web_sys::HtmlInputElement,
    /// The rail's frame, which holds the pointer until it lets go.
    frame: web_sys::Element,
    touch: bool,
    /// Playback the press paused resumes on release.
    resume_playback: bool,
    shown: i64,
    /// The History visit the press belongs to; leaving it ends the hold.
    session: u64,
}

/// The precision cue over a held rail, in pixels from the History bar's top-left.
#[derive(Clone, Copy, PartialEq)]
struct ScrubCue {
    overview: bool,
    timestamp: i64,
    /// Halvings of speed: 0 at full speed, up to [`FINEST_LEVEL`].
    level: u8,
    thumb_x: f64,
    cue_x: f64,
    /// The cue's bottom edge, where the guide from the thumb ends.
    bottom: f64,
    thumb_top: f64,
}

fn place_cue(bar: &web_sys::Element, rail: &Rail, held: &HeldRail, pointer_y: f64) -> ScrubCue {
    let frame = bar.get_bounding_client_rect();
    let timestamp = held.scrub.timestamp();
    let thumb_x = rail.x_of(timestamp as f64) - frame.left();
    let margin = CUE_HALF_WIDTH_PX.min(frame.width() / 2.0);
    let gap = if held.touch {
        CUE_GAP_TOUCH_PX
    } else {
        CUE_GAP_MOUSE_PX
    };
    let bottom = (pointer_y - gap)
        .min(frame.top() - CUE_ABOVE_BAR_PX)
        .max(CUE_HIGHEST_PX)
        - frame.top();
    ScrubCue {
        overview: held.overview,
        timestamp,
        level: precision_scrub::level_for_scale(held.scrub.scale()),
        thumb_x,
        cue_x: thumb_x.clamp(margin, frame.width() - margin),
        bottom,
        thumb_top: rail.center_y - rail.thumb / 2.0 - frame.top(),
    }
}

/// A scrubbed time, to the second a fine scrub can reach.
fn format_scrub_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

pub(crate) fn format_history_timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// The value of a `datetime-local` field for `ts`, in the UTC the map shows.
fn format_time_field(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_default()
}

fn parse_time_field(value: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

/// A rail end, as precise as the rail's span needs: minutes within a couple of days, days
/// within a few months, months beyond.
fn format_rail_edge(ts: i64, span_secs: i64) -> String {
    let format = if span_secs < 2 * DAY_SECS {
        "%m-%d %H:%M"
    } else if span_secs < 120 * DAY_SECS {
        "%Y-%m-%d"
    } else {
        "%Y-%m"
    };
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format(format).to_string())
        .unwrap_or_default()
}

fn clamp_timestamp_to_bounds(ts: i64, bounds: (i64, i64)) -> i64 {
    ts.clamp(bounds.0.min(bounds.1), bounds.0.max(bounds.1))
}

/// Shows the selected time on a rail over `range`, writing the range first so the value is
/// clamped to the range it belongs to. A time beyond the range, such as now after the latest
/// record, shows at that end; no selected time shows the latest.
fn show_on_rail(input: &web_sys::HtmlInputElement, range: (i64, i64), ts: Option<i64>) {
    input.set_min(&range.0.to_string());
    input.set_max(&range.1.to_string());
    input.set_value(&clamp_timestamp_to_bounds(ts.unwrap_or(range.1), range).to_string());
}

fn normalized_focus_span(bounds: (i64, i64), focus_span: Option<i64>) -> Option<i64> {
    let total = bounds.1.saturating_sub(bounds.0);
    let span = focus_span?.clamp(1, total.max(1));
    if span >= total { None } else { Some(span) }
}

fn center_focus_window(bounds: (i64, i64), center: i64, focus_span: Option<i64>) -> (i64, i64) {
    let earliest = bounds.0.min(bounds.1);
    let latest = bounds.0.max(bounds.1);
    let bounds = (earliest, latest);
    let Some(span) = normalized_focus_span(bounds, focus_span) else {
        return bounds;
    };

    let center = clamp_timestamp_to_bounds(center, bounds);
    let max_start = latest.saturating_sub(span);
    let start = center.saturating_sub(span / 2).clamp(earliest, max_start);
    (start, start.saturating_add(span).min(latest))
}

fn normalize_focus_window(
    bounds: (i64, i64),
    selected_ts: i64,
    focus_span: Option<i64>,
    current_window: Option<(i64, i64)>,
    force_recenter: bool,
) -> (i64, i64) {
    let earliest = bounds.0.min(bounds.1);
    let latest = bounds.0.max(bounds.1);
    let bounds = (earliest, latest);
    let Some(span) = normalized_focus_span(bounds, focus_span) else {
        return bounds;
    };

    if force_recenter {
        return center_focus_window(bounds, selected_ts, Some(span));
    }

    let Some((current_start, _)) = current_window else {
        return center_focus_window(bounds, selected_ts, Some(span));
    };

    let max_start = latest.saturating_sub(span);
    let start = current_start.clamp(earliest, max_start);
    let current = (start, start.saturating_add(span).min(latest));
    let selected_ts = clamp_timestamp_to_bounds(selected_ts, bounds);

    if selected_ts < current.0 || selected_ts > current.1 {
        return center_focus_window(bounds, selected_ts, Some(span));
    }

    let margin = (span / 8).max(60).min(span / 2);
    if (selected_ts <= current.0.saturating_add(margin) && current.0 > earliest)
        || (selected_ts >= current.1.saturating_sub(margin) && current.1 < latest)
    {
        center_focus_window(bounds, selected_ts, Some(span))
    } else {
        current
    }
}

fn range_percent(value: i64, bounds: (i64, i64)) -> f64 {
    let earliest = bounds.0.min(bounds.1);
    let latest = bounds.0.max(bounds.1);
    let total = latest.saturating_sub(earliest);
    if total <= 0 {
        return 0.0;
    }
    let clamped = clamp_timestamp_to_bounds(value, (earliest, latest));
    ((clamped - earliest) as f64 / total as f64) * 100.0
}

fn focus_band_style(window: Option<(i64, i64)>, bounds: Option<(i64, i64)>) -> String {
    let Some((focus_start, focus_end)) = window else {
        return "left: 0%; width: 100%;".to_string();
    };
    let Some(bounds) = bounds else {
        return "left: 0%; width: 100%;".to_string();
    };

    let left = range_percent(focus_start, bounds);
    let right = range_percent(focus_end, bounds);
    format!("left: {left:.4}%; width: {:.4}%;", (right - left).max(0.0))
}

/// The lens between the overview's focus band and the detail rail beneath it: a trapezoid from
/// the band's ends down to the rail's.
fn lens_style(window: Option<(i64, i64)>, bounds: Option<(i64, i64)>) -> String {
    let (Some((start, end)), Some(bounds)) = (window, bounds) else {
        return "clip-path: polygon(0 0, 100% 0, 100% 100%, 0 100%);".to_string();
    };
    format!(
        "clip-path: polygon({:.3}% 0, {:.3}% 0, 100% 100%, 0 100%);",
        range_percent(start, bounds),
        range_percent(end, bounds)
    )
}

/// Timeline scrubber bar, visible only in history mode.
#[component]
pub fn Timeline() -> impl IntoView {
    let CurrentMode(mode) = expect_context();
    let HistoryTimestamp(timestamp) = expect_context();
    let PlaybackActive(playing) = expect_context();
    let PlaybackSpeed(speed) = expect_context();
    let HistoryBoundsSignal(bounds) = expect_context();
    let HistoryFetchNonce(history_fetch_nonce) = expect_context();
    let HistoryLegacyGeometryActive(history_legacy_geometry_active) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let TerritoryGeometryStore(geo_store) = expect_context();
    let GuildColorStore(guild_color_store) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let HistoryBarHeight(bar_height) = expect_context();
    let LastLiveSeq(last_live_seq) = expect_context();
    let HistoryBufferedUpdates(history_buffered_updates) = expect_context();
    let HistoryBufferModeActive(history_buffer_mode_active) = expect_context();
    let NeedsLiveResync(needs_live_resync) = expect_context();
    let LiveHandoffResyncCount(live_handoff_resync_count) = expect_context();
    let is_visible = move || mode.get() == MapMode::History;
    let slider_ref = NodeRef::<leptos::html::Input>::new();
    let overview_ref = NodeRef::<leptos::html::Input>::new();
    let HistoryMapTime(history_map_time) = expect_context();
    let fetch_ctx = history::HistoryFetchContext {
        history_map_time,
        mode,
        history_fetch_nonce,
        history_legacy_geometry_active,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
    };

    // While the detail rail is held, its range belongs to the pointer: its window stays put.
    // The overview's thumb is always the selected time; its focus band shows the window, which
    // stops short of the selected time at either end of history.
    let scrub_active: RwSignal<bool> = RwSignal::new(false);
    let focus_span: RwSignal<Option<i64>> = RwSignal::new(DEFAULT_HISTORY_FOCUS_SPAN);
    let focus_window: RwSignal<Option<(i64, i64)>> = RwSignal::new(None);
    let last_focus_span_key = Rc::new(Cell::new(i64::MIN));
    let last_bounds = Rc::new(Cell::new(None::<(i64, i64)>));

    Effect::new(move || {
        if scrub_active.get() {
            return;
        }
        let Some(bounds_now) = bounds.get() else {
            focus_window.set(None);
            return;
        };
        let selected_ts = timestamp.get().unwrap_or(bounds_now.1);
        let span = focus_span.get();
        let span_key = span.unwrap_or(-1);
        let force_recenter =
            last_focus_span_key.get() != span_key || last_bounds.get() != Some(bounds_now);
        let next = normalize_focus_window(
            bounds_now,
            selected_ts,
            span,
            focus_window.get_untracked(),
            force_recenter,
        );
        last_focus_span_key.set(span_key);
        last_bounds.set(Some(bounds_now));
        focus_window.set(Some(next));
    });

    // Each rail takes its range and then the selected time together, whenever either changes.
    // A range input clamps its value to the range it has when the value is set, and again
    // whenever the range changes, so a time set while bounds were still loading (or for
    // another window) would stay clamped to that stale range, at the far left.
    Effect::new(move || {
        let ts = timestamp.get();
        if let (Some(input), Some(range)) = (overview_ref.get(), bounds.get()) {
            show_on_rail(&input, range, ts);
        }
    });
    Effect::new(move || {
        let ts = timestamp.get();
        let range = focus_window.get().or_else(|| bounds.get());
        if !scrub_active.get()
            && let (Some(input), Some(range)) = (slider_ref.get(), range)
        {
            show_on_rail(&input, range, ts);
        }
    });

    // The rail's value, and for the overview the detail window centred on it.
    let rail_value = move |e: &web_sys::Event, overview: bool| -> Option<i64> {
        let input = e.target()?.dyn_into::<web_sys::HtmlInputElement>().ok()?;
        let raw = input.value_as_number();
        if !raw.is_finite() {
            return None;
        }
        let mut val = raw.round() as i64;
        if overview && let Some(bounds_now) = bounds.get_untracked() {
            val = clamp_timestamp_to_bounds(val, bounds_now);
            focus_window.set(Some(center_focus_window(
                bounds_now,
                val,
                focus_span.get_untracked(),
            )));
        }
        playing.set(false);
        Some(val)
    };
    // Keys and assistive technology change a rail through its native input; a pointer holds
    // its frame instead (below).
    let on_range_input = move |e: web_sys::Event| {
        let Some(val) = rail_value(&e, false) else {
            return;
        };
        scrub_active.set(true);
        timestamp.set(Some(val));
        history::preview(val, fetch_ctx);
    };

    let on_range_change = move |e: web_sys::Event| {
        let Some(val) = rail_value(&e, false) else {
            return;
        };
        timestamp.set(Some(val));
        scrub_active.set(false);
        history::fetch_and_apply_with(val, fetch_ctx);
    };

    let on_overview_input = move |e: web_sys::Event| {
        let Some(val) = rail_value(&e, true) else {
            return;
        };
        scrub_active.set(false);
        timestamp.set(Some(val));
        history::preview(val, fetch_ctx);
    };

    let on_overview_change = move |e: web_sys::Event| {
        let Some(val) = rail_value(&e, true) else {
            return;
        };
        scrub_active.set(false);
        timestamp.set(Some(val));
        history::fetch_and_apply_with(val, fetch_ctx);
    };

    let exit = move |_: web_sys::MouseEvent| {
        history::exit_history_mode(history::ExitHistoryModeInput {
            mode,
            playback_active: playing,
            history_fetch_nonce,
            history_timestamp: timestamp,
            history_legacy_geometry_active,
            history_buffered_updates,
            history_buffer_mode_active,
            last_live_seq,
            needs_live_resync,
            live_handoff_resync_count,
            history_sr_leaderboard,
            territories,
        });
    };
    let step = move |delta: i64| {
        let ctx = history::HistoryStepContext {
            history_timestamp: timestamp,
            playback_active: playing,
            fetch: fetch_ctx,
        };
        if delta < 0 {
            history::step_backward(ctx);
        } else {
            history::step_forward(ctx);
        }
    };
    let cycle_speed = move |_: web_sys::MouseEvent| {
        let next = SPEEDS
            .iter()
            .copied()
            .find(|&s| s > speed.get_untracked())
            .unwrap_or(SPEEDS[0]);
        speed.set(next);
    };
    // The sidebar footer carries the History toggle wherever it is on screen; elsewhere the
    // bar offers the way back to Live itself.
    let shows_exit = move || is_mobile.get() || !sidebar_open.get();
    // Where the bar is narrow, steps, speed and zoom open above it from one button.
    let more_open = RwSignal::new(false);

    // The bar's height clears the minimap above it.
    let bar_ref = NodeRef::<leptos::html::Div>::new();
    let resize_watch = StoredValue::new_local(
        None::<(
            web_sys::ResizeObserver,
            wasm_bindgen::closure::Closure<dyn Fn()>,
        )>,
    );
    Effect::new(move || {
        let Some(bar) = bar_ref.get() else {
            return;
        };
        if resize_watch.with_value(Option::is_some) {
            return;
        }
        let measured = bar.clone();
        let resized = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
            bar_height.try_set(f64::from(measured.offset_height()));
        });
        let Ok(observer) =
            web_sys::ResizeObserver::new(wasm_bindgen::JsCast::unchecked_ref(resized.as_ref()))
        else {
            return;
        };
        observer.observe(&bar);
        resize_watch.set_value(Some((observer, resized)));
    });
    on_cleanup(move || {
        if let Some((observer, _resized)) = resize_watch.try_update_value(Option::take).flatten() {
            observer.disconnect();
        }
    });

    // A pointer on a rail drags its thumb itself, finer the higher it rises above the rail
    // (crate::precision_scrub). The rail's frame holds the pointer wherever it goes until it
    // lets go, so the map beneath neither pans nor selects.
    let held = StoredValue::new_local(None::<HeldRail>);
    let cue: RwSignal<Option<ScrubCue>> = RwSignal::new(None);
    // The held rail's time: its thumb, the selected time and, for the overview, the detail
    // window around it. The map follows through the coalesced preview.
    let show_held = move |overview: bool, input: &web_sys::HtmlInputElement, ts: i64| {
        input.set_value(&ts.to_string());
        if overview && let Some(bounds_now) = bounds.get_untracked() {
            focus_window.set(Some(center_focus_window(
                bounds_now,
                ts,
                focus_span.get_untracked(),
            )));
        }
        timestamp.set(Some(ts));
        history::preview(ts, fetch_ctx);
    };
    // Letting go seeks the time shown exactly, as releasing a native rail does, and resumes
    // playback the press paused. Leaving History, or the visit the press belongs to, lets go
    // without either: a seek now would outrun the handoff to Live.
    let let_go = move |settle: bool| {
        let Some(released) = held.try_update_value(Option::take).flatten() else {
            return;
        };
        let pointer_id = released.scrub.pointer_id;
        if released.frame.has_pointer_capture(pointer_id) {
            let _ = released.frame.release_pointer_capture(pointer_id);
        }
        cue.set(None);
        scrub_active.set(false);
        if settle
            && released.session == history::current_session()
            && mode.get_untracked() == MapMode::History
        {
            let ts = released.scrub.timestamp();
            if released.overview
                && let Some(bounds_now) = bounds.get_untracked()
            {
                focus_window.set(Some(center_focus_window(
                    bounds_now,
                    ts,
                    focus_span.get_untracked(),
                )));
            }
            timestamp.set(Some(ts));
            history::fetch_and_apply_with(ts, fetch_ctx);
            if released.resume_playback {
                playing.set(true);
            }
        }
    };
    // Whether the hold is this pointer's. A hold left over from an earlier History visit ends
    // here, before the pointer can move or release it.
    let owns = move |e: &web_sys::PointerEvent| {
        let (mine, stale) = held.with_value(|held| {
            held.as_ref().map_or((false, false), |held| {
                (
                    held.scrub.pointer_id == e.pointer_id(),
                    held.session != history::current_session(),
                )
            })
        });
        if stale {
            let_go(false);
        }
        mine && !stale
    };
    let press = move |e: web_sys::PointerEvent, overview: bool| {
        let mouse = e.pointer_type() == "mouse";
        if !e.is_primary() || (mouse && e.button() != 0) || mode.get_untracked() != MapMode::History
        {
            return;
        }
        // One pointer holds a rail at a time; one its frame no longer holds has let go.
        if held.with_value(|held| {
            held.as_ref()
                .is_some_and(|held| held.frame.has_pointer_capture(held.scrub.pointer_id))
        }) {
            return;
        }
        let_go(true);
        let input_ref = if overview { overview_ref } else { slider_ref };
        let (Some(input), Some(frame)) = (
            input_ref.get_untracked(),
            e.current_target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok()),
        ) else {
            return;
        };
        let (Some(rail), Some(bar)) = (rail_of(&input), bar_ref.get_untracked()) else {
            return;
        };
        let value = input.value_as_number();
        if !value.is_finite() {
            return;
        }
        if mouse {
            // No text selection or focus change from the press; the rail takes focus itself.
            // Touch presses stay default: `touch-action: none` keeps the page still.
            e.prevent_default();
        }
        let _ = input.focus();
        let _ = frame.set_pointer_capture(e.pointer_id());

        let (x, y) = client_position(&e);
        let slack = if mouse {
            THUMB_SLACK_MOUSE_PX
        } else {
            THUMB_SLACK_TOUCH_PX
        };
        let start = value.round() as i64;
        let scrub = Scrub::press(e.pointer_id(), x, y, &rail, start, slack);
        let shown = scrub.timestamp();
        let pressed = HeldRail {
            scrub,
            overview,
            input,
            frame,
            touch: !mouse,
            resume_playback: playing.get_untracked(),
            shown,
            session: history::current_session(),
        };
        playing.set(false);
        scrub_active.set(!overview);
        if shown != start {
            show_held(overview, &pressed.input, shown);
        }
        cue.set(Some(place_cue(&bar, &rail, &pressed, y)));
        held.set_value(Some(pressed));
    };
    // Moves the held rail's pointer to the event's position: the rail as laid out now, and the
    // time it shows if that changed.
    let follow = move |e: &web_sys::PointerEvent| {
        let (x, y) = client_position(e);
        held.try_update_value(|held| {
            let held = held.as_mut()?;
            let rail = rail_of(&held.input)?;
            let ts = held.scrub.move_to(x, y, &rail);
            let step = (ts != held.shown).then(|| (held.overview, held.input.clone(), ts));
            held.shown = ts;
            Some((rail, step))
        })
        .flatten()
    };
    let drag = move |e: web_sys::PointerEvent| {
        if !owns(&e) {
            return;
        }
        // A mouse whose button came up where the page heard no release has let go where it
        // last moved.
        if e.pointer_type() == "mouse" && (e.buttons() & 1) == 0 {
            let_go(true);
            return;
        }
        let Some((rail, step)) = follow(&e) else {
            return;
        };
        let y = client_position(&e).1;
        let next_cue = bar_ref.get_untracked().and_then(|bar| {
            held.with_value(|held| held.as_ref().map(|held| place_cue(&bar, &rail, held, y)))
        });
        if next_cue.is_some() {
            cue.set(next_cue);
        }
        if let Some((overview, input, ts)) = step {
            show_held(overview, &input, ts);
        }
    };
    // A release may land away from the last move; its own position chooses the time.
    let release = move |e: web_sys::PointerEvent| {
        if owns(&e) {
            let _ = follow(&e);
            let_go(true);
        }
    };
    // A cancelled or lost pointer reports no position worth taking: the last move's time stands.
    let lose = move |e: web_sys::PointerEvent| {
        if owns(&e) {
            let_go(true);
        }
    };
    // Leaving History lets go without a seek; the phone sheet covering the bar settles.
    Effect::new(move || {
        let history = mode.get() == MapMode::History;
        let covered = is_mobile.get() && sidebar_open.get();
        if (!history || covered) && held.with_value(Option::is_some) {
            let_go(history);
        }
    });
    on_cleanup(move || {
        if let Some(released) = held.try_update_value(Option::take).flatten() {
            let _ = released
                .frame
                .release_pointer_capture(released.scrub.pointer_id);
        }
    });
    let held_level = move |overview: bool| {
        cue.with(|cue| {
            cue.filter(|cue| cue.overview == overview)
                .map(|cue| cue.level)
        })
    };
    let cue_level = move || cue.with(|cue| cue.map_or(0, |cue| cue.level));
    let cue_px = move |pick: fn(&ScrubCue) -> f64| {
        cue.with(|cue| {
            cue.as_ref()
                .map(|cue| format!("{:.1}px", pick(cue)))
                .unwrap_or_default()
        })
    };

    // The time field follows the selected time, except while it is being edited.
    let time_ref = NodeRef::<leptos::html::Input>::new();
    let editing = move |input: &web_sys::HtmlInputElement| {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.active_element())
            .is_some_and(|active| active == ***input)
    };
    let sync_time_field = move || {
        if let Some(input) = time_ref.get_untracked() {
            input.set_value(
                &timestamp
                    .get_untracked()
                    .map(format_time_field)
                    .unwrap_or_default(),
            );
        }
    };
    Effect::new(move || {
        timestamp.track();
        if let Some(input) = time_ref.get()
            && !editing(&input)
        {
            sync_time_field();
        }
    });
    let on_time_change = move |e: web_sys::Event| {
        let (Some(value), Some(bounds_now)) = (
            e.target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|input| input.value()),
            bounds.get_untracked(),
        ) else {
            return;
        };
        let Some(ts) = parse_time_field(&value) else {
            return;
        };
        let ts = clamp_timestamp_to_bounds(ts, bounds_now);
        playing.set(false);
        timestamp.set(Some(ts));
        focus_window.set(Some(center_focus_window(
            bounds_now,
            ts,
            focus_span.get_untracked(),
        )));
        history::fetch_and_apply_with(ts, fetch_ctx);
    };

    let outside = window_event_listener(leptos::ev::pointerdown, move |event| {
        if !more_open.get_untracked() {
            return;
        }
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .zip(bar_ref.get_untracked())
            .is_some_and(|(target, bar)| bar.contains(Some(&target)));
        if !inside {
            more_open.set(false);
        }
    });
    on_cleanup(move || outside.remove());

    let rail_window = move || focus_window.get().or_else(|| bounds.get());
    let edge = move |window: Option<(i64, i64)>, pick: fn((i64, i64)) -> i64| {
        window
            .map(|window| format_rail_edge(pick(window), window.1 - window.0))
            .unwrap_or_default()
    };

    view! {
        <div
            node_ref=bar_ref
            class="timeline-bar hud-enter"
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Escape" && more_open.get_untracked() {
                    // Closing the panel is all this Escape does; the map keeps its selection.
                    event.prevent_default();
                    more_open.set(false);
                }
            }
            role="group"
            aria-label="History"
            style:display=move || {
                if !is_visible() || (is_mobile.get() && sidebar_open.get()) { "none" } else { "block" }
            }
            style:right=move || {
                if !is_mobile.get() && sidebar_open.get() {
                    format!("{:.0}px", sidebar_width.get())
                } else {
                    "0".to_string()
                }
            }
        >
            <Show when=move || history_legacy_geometry_active.get()>
                <p class="tl-note">"Pre-Sequoia scrape data may be inaccurate."</p>
            </Show>
            <div class="tl-head">
                <button
                    type="button"
                    class="tl-icon tl-play timeline-play"
                    title=move || if playing.get() { "Pause (Space)" } else { "Play (Space)" }
                    aria-label=move || if playing.get() { "Pause" } else { "Play" }
                    inner_html=move || if playing.get() { PAUSE_SVG } else { PLAY_SVG }
                    on:click=move |_| playing.update(|v| *v = !*v)
                />
                <input
                    node_ref=time_ref
                    type="datetime-local"
                    class="tl-time"
                    aria-label="Selected time"
                    step="60"
                    min=move || bounds.get().map(|(e, _)| format_time_field(e)).unwrap_or_default()
                    max=move || bounds.get().map(|(_, l)| format_time_field(l)).unwrap_or_default()
                    on:change=on_time_change
                    on:blur=move |_| sync_time_field()
                />
                <button
                    type="button"
                    class="tl-icon tl-more"
                    aria-label="More history controls"
                    aria-controls="tl-extras"
                    aria-expanded=move || more_open.get().to_string()
                    inner_html=MORE_SVG
                    on:click=move |_| more_open.update(|open| *open = !*open)
                />
                <div id="tl-extras" class="tl-extras" class:tl-extras-open=move || more_open.get()>
                    <div class="tl-steps">
                    <button
                        type="button"
                        class="tl-icon timeline-step"
                        title="Step back ([)"
                        aria-label="Step back"
                        inner_html=STEP_BACK_SVG
                        on:click=move |_| step(-1)
                    />
                    <button
                        type="button"
                        class="tl-icon timeline-step"
                        title="Step forward (])"
                        aria-label="Step forward"
                        inner_html=STEP_FORWARD_SVG
                        on:click=move |_| step(1)
                    />
                    </div>
                    <button
                        type="button"
                        class="tl-speed timeline-speed"
                        aria-label=move || format!("Playback speed {}x", speed.get() as i32)
                        title="Playback speed"
                        on:click=cycle_speed
                    >
                        {move || format!("{}x", speed.get() as i32)}
                    </button>
                    <div class="tl-zoom" role="group" aria-label="Timeline zoom">
                        {HISTORY_FOCUS_SPANS.iter().map(|&(span, label)| {
                            view! {
                                <button
                                    type="button"
                                    class="tl-zoom-option"
                                    title=format!("Set timeline zoom to {label}")
                                    aria-pressed=move || (focus_span.get() == span).to_string()
                                    on:click=move |_| {
                                        focus_span.set(span);
                                        if let Some(bounds_now) = bounds.get_untracked() {
                                            let selected = timestamp.get_untracked().unwrap_or(bounds_now.1);
                                            focus_window.set(Some(center_focus_window(bounds_now, selected, span)));
                                        }
                                    }
                                >
                                    {label}
                                </button>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                </div>
                <Show when=shows_exit>
                    <button
                        type="button"
                        class="tl-exit"
                        title="Exit history mode (h)"
                        aria-label="Exit history"
                        on:click=exit
                    >
                        <span aria-hidden="true" inner_html=LIVE_SVG />
                        "Live"
                    </button>
                </Show>
            </div>

            <div class="tl-rails">
                <span class="tl-edge tl-overview-start">{move || edge(bounds.get(), |w| w.0)}</span>
                <div
                    class="tl-overview tl-scrub"
                    class:tl-scrub-held=move || held_level(true).is_some()
                    class:tl-scrub-fine=move || held_level(true).is_some_and(|level| level > 0)
                    on:pointerdown=move |e| press(e, true)
                    on:pointermove=drag
                    on:pointerup=release
                    on:pointercancel=lose
                    on:lostpointercapture=lose
                >
                    <div class="timeline-focus-band" style=move || focus_band_style(focus_window.get(), bounds.get()) />
                    <input
                        node_ref=overview_ref
                        type="range"
                        class="timeline-slider timeline-overview-slider"
                        aria-label="Whole history"
                        min="0"
                        max="0"
                        step="1"
                        on:input=on_overview_input
                        on:change=on_overview_change
                    />
                </div>
                <span class="tl-edge tl-overview-end">{move || edge(bounds.get(), |w| w.1)}</span>

                <div class="tl-lens" aria-hidden="true" style=move || lens_style(focus_window.get(), bounds.get()) />

                <span class="tl-edge tl-fine-start">{move || edge(rail_window(), |w| w.0)}</span>
                <div
                    class="tl-fine tl-scrub"
                    class:tl-scrub-held=move || held_level(false).is_some()
                    class:tl-scrub-fine=move || held_level(false).is_some_and(|level| level > 0)
                    on:pointerdown=move |e| press(e, false)
                    on:pointermove=drag
                    on:pointerup=release
                    on:pointercancel=lose
                    on:lostpointercapture=lose
                >
                    <input
                        node_ref=slider_ref
                        type="range"
                        class="timeline-slider"
                        aria-label="Selected time"
                        aria-valuetext=move || timestamp.get().map(format_history_timestamp).unwrap_or_default()
                        min="0"
                        max="0"
                        step="1"
                        on:input=on_range_input
                        on:change=on_range_change
                    />
                </div>
                <span class="tl-edge tl-fine-end">{move || edge(rail_window(), |w| w.1)}</span>
            </div>

            // While a rail is held: the time to the second and how fine the drag is, above the
            // pointer, with a guide down to the thumb once the drag is finer than the rail.
            <Show when=move || cue.with(Option::is_some)>
                <div
                    class="tl-scrub-guide"
                    aria-hidden="true"
                    style:display=move || { if cue_level() > 0 { "block" } else { "none" } }
                    style:left=move || cue_px(|cue| cue.thumb_x)
                    style:top=move || cue_px(|cue| cue.bottom)
                    style:height=move || cue_px(|cue| (cue.thumb_top - cue.bottom).max(0.0))
                />
                <div
                    class="tl-scrub-cue"
                    class:tl-scrub-cue-fine=move || { cue_level() > 0 }
                    aria-hidden="true"
                    style:left=move || cue_px(|cue| cue.cue_x)
                    style:top=move || cue_px(|cue| cue.bottom)
                >
                    <span class="tl-scrub-time">
                        {move || cue.with(|cue| cue.map(|cue| format_scrub_time(cue.timestamp)).unwrap_or_default())}
                    </span>
                    <span class="tl-scrub-hint"><span inner_html=LIFT_SVG />"finer"</span>
                    <span class="tl-scrub-level">
                        {(0..FINEST_LEVEL).map(|step| view! {
                            <i class:tl-scrub-on=move || { cue_level() > step } />
                        }).collect::<Vec<_>>()}
                    </span>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_window_centers_and_clamps_to_bounds() {
        let bounds = (0, 10_000);

        assert_eq!(
            center_focus_window(bounds, 5_000, Some(1_000)),
            (4_500, 5_500)
        );
        assert_eq!(center_focus_window(bounds, 100, Some(1_000)), (0, 1_000));
        assert_eq!(
            center_focus_window(bounds, 9_900, Some(1_000)),
            (9_000, 10_000)
        );
    }

    #[test]
    fn focus_window_uses_full_bounds_for_all_or_short_ranges() {
        let bounds = (0, 10_000);

        assert_eq!(center_focus_window(bounds, 5_000, None), bounds);
        assert_eq!(center_focus_window(bounds, 5_000, Some(20_000)), bounds);
    }

    #[test]
    fn focus_window_preserves_current_window_while_timestamp_is_inside() {
        let bounds = (0, 10_000);

        assert_eq!(
            normalize_focus_window(bounds, 4_000, Some(2_000), Some((3_000, 5_000)), false),
            (3_000, 5_000)
        );
    }

    #[test]
    fn focus_window_recenters_when_timestamp_leaves_or_nears_edge() {
        let bounds = (0, 10_000);

        assert_eq!(
            normalize_focus_window(bounds, 7_500, Some(2_000), Some((3_000, 5_000)), false),
            (6_500, 8_500)
        );
        assert_eq!(
            normalize_focus_window(bounds, 4_800, Some(2_000), Some((3_000, 5_000)), false),
            (3_800, 5_800)
        );
    }
}
