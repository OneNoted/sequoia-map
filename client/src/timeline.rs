use leptos::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::JsCast;

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBarHeight, HistoryBoundsSignal, HistoryBufferModeActive,
    HistoryBufferedUpdates, HistoryFetchNonce, HistoryLegacyGeometryActive, HistoryMapTime,
    HistorySeasonLeaderboard, HistorySeasonScalarSample, HistoryTimestamp, IsMobile, LastLiveSeq,
    LiveHandoffResyncCount, MapMode, NeedsLiveResync, PlaybackActive, PlaybackSpeed, SidebarOpen,
    SidebarWidth, TerritoryGeometryStore,
};
use crate::history;
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
const DEFAULT_HISTORY_FOCUS_SPAN: Option<i64> = Some(6 * HOUR_SECS);

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

fn overview_value_for_window(window: (i64, i64)) -> i64 {
    window.0 + (window.1.saturating_sub(window.0) / 2)
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
    let slider_ref_sync = slider_ref;
    let overview_ref_sync = overview_ref;
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

    // While a rail is held, its range and value belong to the pointer: the detail rail's
    // window stays put and the overview's value is not rewritten until release.
    let scrub_active: RwSignal<bool> = RwSignal::new(false);
    let overview_active: RwSignal<bool> = RwSignal::new(false);
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

    Effect::new(move || {
        let ts = timestamp.get().unwrap_or(0);
        if !scrub_active.get()
            && let Some(input) = slider_ref_sync.get()
        {
            input.set_value(&ts.to_string());
        }
        if !overview_active.get()
            && let Some(input) = overview_ref_sync.get()
        {
            let overview_value = if focus_span.get().is_none() {
                ts
            } else {
                focus_window
                    .get()
                    .map(overview_value_for_window)
                    .unwrap_or(ts)
            };
            input.set_value(&overview_value.to_string());
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
    // A drag that ends where it began fires no change, so releasing the pointer also ends it.
    // The overview lets go a task later: the release's change, dispatched in this same task,
    // must read the released value before the effect rewrites the overview.
    let release = move |_: web_sys::PointerEvent| scrub_active.set(false);
    let overview_release = move |_: web_sys::PointerEvent| {
        gloo_timers::callback::Timeout::new(0, move || {
            overview_active.try_set(false);
        })
        .forget();
    };

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
        overview_active.set(true);
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
        overview_active.set(false);
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
                <div class="tl-overview">
                    <div class="timeline-focus-band" style=move || focus_band_style(focus_window.get(), bounds.get()) />
                    <input
                        node_ref=overview_ref
                        type="range"
                        class="timeline-slider timeline-overview-slider"
                        aria-label="Whole history"
                        min=move || bounds.get().map(|(e, _)| e.to_string()).unwrap_or_else(|| "0".to_string())
                        max=move || bounds.get().map(|(_, l)| l.to_string()).unwrap_or_else(|| "0".to_string())
                        step="1"
                        value=focus_window
                            .get_untracked()
                            .and_then(|window| {
                                if focus_span.get_untracked().is_none() {
                                    None
                                } else {
                                    Some(overview_value_for_window(window))
                                }
                            })
                            .unwrap_or_else(|| timestamp.get_untracked().unwrap_or(0))
                            .to_string()
                        on:input=on_overview_input
                        on:change=on_overview_change
                        on:pointerup=overview_release
                        on:pointercancel=overview_release
                    />
                </div>
                <span class="tl-edge tl-overview-end">{move || edge(bounds.get(), |w| w.1)}</span>

                <div class="tl-lens" aria-hidden="true" style=move || lens_style(focus_window.get(), bounds.get()) />

                <span class="tl-edge tl-fine-start">{move || edge(rail_window(), |w| w.0)}</span>
                <input
                    node_ref=slider_ref
                    type="range"
                    class="timeline-slider tl-fine"
                    aria-label="Selected time"
                    aria-valuetext=move || timestamp.get().map(format_history_timestamp).unwrap_or_default()
                    min=move || rail_window().map(|(start, _)| start.to_string()).unwrap_or_else(|| "0".to_string())
                    max=move || rail_window().map(|(_, end)| end.to_string()).unwrap_or_else(|| "0".to_string())
                    step="1"
                    value=timestamp.get_untracked().unwrap_or(0).to_string()
                    on:input=on_range_input
                    on:change=on_range_change
                    on:pointerup=release
                    on:pointercancel=release
                />
                <span class="tl-edge tl-fine-end">{move || edge(rail_window(), |w| w.1)}</span>
            </div>
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
