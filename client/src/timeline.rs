use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::JsCast;

use crate::app::{
    CurrentMode, GuildColorStore, HistoryBoundsSignal, HistoryBufferModeActive,
    HistoryBufferedUpdates, HistoryFetchNonce, HistoryLegacyGeometryActive,
    HistorySeasonLeaderboard, HistorySeasonScalarSample, HistoryTimestamp, IsMobile, LastLiveSeq,
    LiveHandoffResyncCount, MapMode, NeedsLiveResync, PlaybackActive, PlaybackSpeed, SidebarOpen,
    SidebarWidth, TerritoryGeometryStore,
};
use crate::history;
use crate::territory::ClientTerritoryMap;
use gloo_timers::callback::Timeout;

const HOUR_SECS: i64 = 60 * 60;
const DAY_SECS: i64 = 24 * HOUR_SECS;
const HISTORY_FOCUS_SPANS: &[(Option<i64>, &str, bool)] = &[
    (Some(HOUR_SECS), "1h", false),
    (Some(6 * HOUR_SECS), "6h", false),
    (Some(DAY_SECS), "24h", false),
    (Some(7 * DAY_SECS), "7d", false),
    (Some(30 * DAY_SECS), "30d", false),
    (None, "All", true),
];
const DEFAULT_HISTORY_FOCUS_SPAN: Option<i64> = Some(6 * HOUR_SECS);

fn format_history_timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
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
    if selected_ts <= current.0.saturating_add(margin) && current.0 > earliest {
        center_focus_window(bounds, selected_ts, Some(span))
    } else if selected_ts >= current.1.saturating_sub(margin) && current.1 < latest {
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
    let LastLiveSeq(last_live_seq) = expect_context();
    let HistoryBufferedUpdates(history_buffered_updates) = expect_context();
    let HistoryBufferModeActive(history_buffer_mode_active) = expect_context();
    let NeedsLiveResync(needs_live_resync) = expect_context();
    let LiveHandoffResyncCount(live_handoff_resync_count) = expect_context();
    let is_visible = move || mode.get() == MapMode::History;
    let slider_ref = NodeRef::<leptos::html::Input>::new();
    let overview_ref = NodeRef::<leptos::html::Input>::new();
    let slider_ref_sync = slider_ref.clone();
    let overview_ref_sync = overview_ref.clone();
    let fetch_ctx = history::HistoryFetchContext {
        mode,
        history_fetch_nonce,
        history_legacy_geometry_active,
        history_scalar_sample,
        history_sr_leaderboard,
        geo_store,
        guild_color_store,
        territories,
    };

    // Throttle history fetches while scrubbing so updates remain live without flooding requests.
    let throttle_timeout = Rc::new(RefCell::new(None::<Timeout>));
    let pending_fetch_val = Rc::new(Cell::new(None::<i64>));
    let last_fetch_at_ms = Rc::new(Cell::new(0.0));
    let scrub_active: RwSignal<bool> = RwSignal::new(false);
    let focus_span: RwSignal<Option<i64>> = RwSignal::new(DEFAULT_HISTORY_FOCUS_SPAN);
    let focus_window: RwSignal<Option<(i64, i64)>> = RwSignal::new(None);
    let last_focus_span_key = Rc::new(Cell::new(i64::MIN));
    let last_bounds = Rc::new(Cell::new(None::<(i64, i64)>));

    Effect::new(move || {
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
        if let Some(input) = overview_ref_sync.get() {
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

    let on_range_input = {
        let throttle_timeout = Rc::clone(&throttle_timeout);
        let pending_fetch_val = Rc::clone(&pending_fetch_val);
        let last_fetch_at_ms = Rc::clone(&last_fetch_at_ms);
        move |e: web_sys::Event| {
            let Some(target) = e.target() else {
                return;
            };
            let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                return;
            };
            let raw = input.value_as_number();
            if !raw.is_finite() {
                return;
            }
            let val = raw.round() as i64;

            playing.set(false);
            scrub_active.set(true);
            timestamp.set(Some(val));

            let now_ms = js_sys::Date::now();
            let elapsed_ms = now_ms - last_fetch_at_ms.get();
            if elapsed_ms >= 150.0 && throttle_timeout.borrow().is_none() {
                last_fetch_at_ms.set(now_ms);
                history::fetch_and_apply_with(val, fetch_ctx);
                return;
            }

            pending_fetch_val.set(Some(val));
            if throttle_timeout.borrow().is_some() {
                return;
            }

            let wait_ms = (150.0 - elapsed_ms).max(0.0).round() as u32;
            let throttle_timeout_cb = Rc::clone(&throttle_timeout);
            let pending_fetch_val_cb = Rc::clone(&pending_fetch_val);
            let last_fetch_at_ms_cb = Rc::clone(&last_fetch_at_ms);
            let timeout = Timeout::new(wait_ms, move || {
                let _ = throttle_timeout_cb.borrow_mut().take();
                if let Some(next_val) = pending_fetch_val_cb.take() {
                    last_fetch_at_ms_cb.set(js_sys::Date::now());
                    history::fetch_and_apply_with(next_val, fetch_ctx);
                }
            });
            *throttle_timeout.borrow_mut() = Some(timeout);
        }
    };

    let on_range_change = {
        let throttle_timeout = Rc::clone(&throttle_timeout);
        let pending_fetch_val = Rc::clone(&pending_fetch_val);
        let last_fetch_at_ms = Rc::clone(&last_fetch_at_ms);
        move |e: web_sys::Event| {
            let Some(target) = e.target() else {
                return;
            };
            let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                return;
            };
            let raw = input.value_as_number();
            if !raw.is_finite() {
                return;
            }
            let val = raw.round() as i64;

            playing.set(false);
            timestamp.set(Some(val));
            scrub_active.set(false);

            if let Some(timeout) = throttle_timeout.borrow_mut().take() {
                timeout.cancel();
            }
            pending_fetch_val.set(None);
            last_fetch_at_ms.set(js_sys::Date::now());
            history::fetch_and_apply_with(val, fetch_ctx);
        }
    };

    let on_overview_input = {
        let throttle_timeout = Rc::clone(&throttle_timeout);
        let pending_fetch_val = Rc::clone(&pending_fetch_val);
        let last_fetch_at_ms = Rc::clone(&last_fetch_at_ms);
        move |e: web_sys::Event| {
            let Some(target) = e.target() else {
                return;
            };
            let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                return;
            };
            let raw = input.value_as_number();
            if !raw.is_finite() {
                return;
            }
            let mut val = raw.round() as i64;
            if let Some(bounds_now) = bounds.get_untracked() {
                val = clamp_timestamp_to_bounds(val, bounds_now);
                focus_window.set(Some(center_focus_window(
                    bounds_now,
                    val,
                    focus_span.get_untracked(),
                )));
            }

            playing.set(false);
            scrub_active.set(false);
            timestamp.set(Some(val));

            let now_ms = js_sys::Date::now();
            let elapsed_ms = now_ms - last_fetch_at_ms.get();
            if elapsed_ms >= 150.0 && throttle_timeout.borrow().is_none() {
                last_fetch_at_ms.set(now_ms);
                history::fetch_and_apply_with(val, fetch_ctx);
                return;
            }

            pending_fetch_val.set(Some(val));
            if throttle_timeout.borrow().is_some() {
                return;
            }

            let wait_ms = (150.0 - elapsed_ms).max(0.0).round() as u32;
            let throttle_timeout_cb = Rc::clone(&throttle_timeout);
            let pending_fetch_val_cb = Rc::clone(&pending_fetch_val);
            let last_fetch_at_ms_cb = Rc::clone(&last_fetch_at_ms);
            let timeout = Timeout::new(wait_ms, move || {
                let _ = throttle_timeout_cb.borrow_mut().take();
                if let Some(next_val) = pending_fetch_val_cb.take() {
                    last_fetch_at_ms_cb.set(js_sys::Date::now());
                    history::fetch_and_apply_with(next_val, fetch_ctx);
                }
            });
            *throttle_timeout.borrow_mut() = Some(timeout);
        }
    };

    let on_overview_change = {
        let throttle_timeout = Rc::clone(&throttle_timeout);
        let pending_fetch_val = Rc::clone(&pending_fetch_val);
        let last_fetch_at_ms = Rc::clone(&last_fetch_at_ms);
        move |e: web_sys::Event| {
            let Some(target) = e.target() else {
                return;
            };
            let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                return;
            };
            let raw = input.value_as_number();
            if !raw.is_finite() {
                return;
            }
            let mut val = raw.round() as i64;
            if let Some(bounds_now) = bounds.get_untracked() {
                val = clamp_timestamp_to_bounds(val, bounds_now);
                focus_window.set(Some(center_focus_window(
                    bounds_now,
                    val,
                    focus_span.get_untracked(),
                )));
            }

            playing.set(false);
            scrub_active.set(false);
            timestamp.set(Some(val));

            if let Some(timeout) = throttle_timeout.borrow_mut().take() {
                timeout.cancel();
            }
            pending_fetch_val.set(None);
            last_fetch_at_ms.set(js_sys::Date::now());
            history::fetch_and_apply_with(val, fetch_ctx);
        }
    };

    let speed_options: &[f64] = &[1.0, 10.0, 60.0, 360.0];

    // SVG icon constants
    let play_svg = r#"<svg width="12" height="14" viewBox="0 0 12 14" fill="currentColor" xmlns="http://www.w3.org/2000/svg"><path d="M1 1.5v11l10-5.5z"/></svg>"#;
    let pause_svg = r#"<svg width="12" height="14" viewBox="0 0 12 14" fill="currentColor" xmlns="http://www.w3.org/2000/svg"><rect x="1" y="1" width="3.5" height="12" rx="0.75"/><rect x="7.5" y="1" width="3.5" height="12" rx="0.75"/></svg>"#;
    let skip_back_svg = r#"<svg width="14" height="12" viewBox="0 0 14 12" fill="currentColor" xmlns="http://www.w3.org/2000/svg"><rect x="1" y="1" width="2" height="10" rx="0.5"/><path d="M13 1v10L5.5 6z"/></svg>"#;
    let skip_fwd_svg = r#"<svg width="14" height="12" viewBox="0 0 14 12" fill="currentColor" xmlns="http://www.w3.org/2000/svg"><rect x="11" y="1" width="2" height="10" rx="0.5"/><path d="M1 1v10l7.5-5z"/></svg>"#;

    // Cycle speed on mobile tap
    let cycle_speed = move |_: web_sys::MouseEvent| {
        let current = speed.get_untracked();
        let next = match current as i32 {
            1 => 10.0,
            10 => 60.0,
            60 => 360.0,
            _ => 1.0,
        };
        speed.set(next);
    };

    view! {
        <>
        <div
            style:display=move || {
                if !is_visible()
                    || !history_legacy_geometry_active.get()
                    || (is_mobile.get() && sidebar_open.get())
                {
                    "none"
                } else {
                    "flex"
                }
            }
            style:right=move || {
                if !is_mobile.get() && sidebar_open.get() {
                    format!("{:.0}px", sidebar_width.get())
                } else {
                    "0".to_string()
                }
            }
            style:bottom=move || if is_mobile.get() { "142px" } else { "88px" }
            style="position: absolute; left: 0; z-index: 26; margin: 0 12px; padding: 8px 12px; border: 1px solid rgba(245,197,66,0.32); border-radius: 8px; background: rgba(19,22,31,0.96); color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.68rem; line-height: 1.45; box-shadow: 0 10px 28px rgba(0,0,0,0.35);"
        >
            {move || {
                let cutoff = history::rekindled_world_release_secs();
                let cutoff_label = format_history_timestamp(cutoff);
                format!(
                    "Selected time predates Rekindled World ({cutoff_label} UTC). Sequoia only has current territory geometry, so older history can render with misplaced or missing territories."
                )
            }}
        </div>
        <div
            class="timeline-bar hud-enter"
            style:display=move || {
                if !is_visible() || (is_mobile.get() && sidebar_open.get()) {
                    "none"
                } else {
                    "flex"
                }
            }
            style:right=move || {
                if !is_mobile.get() && sidebar_open.get() {
                    format!("{:.0}px", sidebar_width.get())
                } else {
                    "0".to_string()
                }
            }
            style:flex-direction=move || if is_mobile.get() { "column" } else { "row" }
            style:height=move || if is_mobile.get() { "auto" } else { "84px" }
            style:padding=move || if is_mobile.get() { "8px 12px" } else { "8px 16px" }
            style:gap=move || if is_mobile.get() { "7px" } else { "10px" }
            style="position: absolute; bottom: 0; left: 0; z-index: 25; background: #13161f; border-top: 1px solid rgba(245,197,66,0.15); align-items: stretch; font-family: 'JetBrains Mono', monospace; font-size: 0.72rem; transition: right 0.3s cubic-bezier(0.4, 0, 0.2, 1);"
        >
            // --- Row 1: Transport + Speed controls ---
            <div
                style:width=move || if is_mobile.get() { "100%" } else { "auto" }
                style="display: flex; align-items: center; gap: 0; min-width: 0; flex-shrink: 0;"
            >
                // Play/Pause button
                <button
                    title=move || if playing.get() { "Pause (Space)" } else { "Play (Space)" }
                    style:min-width=move || if is_mobile.get() { "44px" } else { "32px" }
                    style:min-height=move || if is_mobile.get() { "44px" } else { "32px" }
                    style:width=move || if is_mobile.get() { "44px" } else { "32px" }
                    style:height=move || if is_mobile.get() { "44px" } else { "32px" }
                    style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 6px; cursor: pointer; color: #f5c542; font-size: 0.9rem; display: flex; align-items: center; justify-content: center; flex-shrink: 0; transition: background 0.15s ease, border-color 0.15s ease; touch-action: manipulation;"
                    inner_html=move || if playing.get() { pause_svg } else { play_svg }
                    on:click=move |_| playing.update(|v| *v = !*v)
                    on:mouseenter=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#232738").ok();
                            el.style().set_property("border-color", "#3a3f5c").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#1a1d2a").ok();
                            el.style().set_property("border-color", "#282c3e").ok();
                        }
                    }
                />

                // Step backward
                <button
                    title="Step back ([)"
                    style:min-width=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:min-height=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:width=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:height=move || if is_mobile.get() { "44px" } else { "28px" }
                    style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; cursor: pointer; color: #9a9590; font-size: 0.75rem; display: flex; align-items: center; justify-content: center; flex-shrink: 0; margin-left: 4px; transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease; touch-action: manipulation;"
                    inner_html=skip_back_svg
                    on:click=move |_| {
                        history::step_backward(history::HistoryStepContext {
                            history_timestamp: timestamp,
                            playback_active: playing,
                            fetch: history::HistoryFetchContext {
                                mode,
                                history_fetch_nonce,
                                history_legacy_geometry_active,
                                history_scalar_sample,
                                history_sr_leaderboard,
                                geo_store,
                                guild_color_store,
                                territories,
                            },
                        });
                    }
                    on:mouseenter=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#232738").ok();
                            el.style().set_property("color", "#e2e0d8").ok();
                            el.style().set_property("border-color", "#3a3f5c").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#1a1d2a").ok();
                            el.style().set_property("color", "#9a9590").ok();
                            el.style().set_property("border-color", "#282c3e").ok();
                        }
                    }
                />

                // Step forward
                <button
                    title="Step forward (])"
                    style:min-width=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:min-height=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:width=move || if is_mobile.get() { "44px" } else { "28px" }
                    style:height=move || if is_mobile.get() { "44px" } else { "28px" }
                    style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; cursor: pointer; color: #9a9590; font-size: 0.75rem; display: flex; align-items: center; justify-content: center; flex-shrink: 0; margin-left: 4px; transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease; touch-action: manipulation;"
                    inner_html=skip_fwd_svg
                    on:click=move |_| {
                        history::step_forward(history::HistoryStepContext {
                            history_timestamp: timestamp,
                            playback_active: playing,
                            fetch: history::HistoryFetchContext {
                                mode,
                                history_fetch_nonce,
                                history_legacy_geometry_active,
                                history_scalar_sample,
                                history_sr_leaderboard,
                                geo_store,
                                guild_color_store,
                                territories,
                            },
                        });
                    }
                    on:mouseenter=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#232738").ok();
                            el.style().set_property("color", "#e2e0d8").ok();
                            el.style().set_property("border-color", "#3a3f5c").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("background", "#1a1d2a").ok();
                            el.style().set_property("color", "#9a9590").ok();
                            el.style().set_property("border-color", "#282c3e").ok();
                        }
                    }
                />

                // Divider: transport | speed
                <div style="width: 1px; height: 24px; background: #282c3e; margin: 0 10px; flex-shrink: 0;" />

                // Speed selector — desktop: <select>, mobile: cycle button
                {move || {
                    if is_mobile.get() {
                        view! {
                            <button
                                style="min-width: 44px; min-height: 44px; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #9a9590; font-size: 0.68rem; padding: 4px 8px; cursor: pointer; font-family: 'JetBrains Mono', monospace; flex-shrink: 0; transition: border-color 0.15s ease, color 0.15s ease; touch-action: manipulation;"
                                on:click=cycle_speed
                            >
                                {move || format!("{}x", speed.get() as i32)}
                            </button>
                        }.into_any()
                    } else {
                        view! {
                            <select
                                prop:value=move || speed.get().to_string()
                                style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #9a9590; font-size: 0.68rem; padding: 4px 6px; cursor: pointer; font-family: 'JetBrains Mono', monospace; flex-shrink: 0; outline: none; transition: border-color 0.15s ease, color 0.15s ease;"
                                on:change=move |e| {
                                    let Some(target) = e.target() else {
                                        return;
                                    };
                                    let Ok(target) = target.dyn_into::<web_sys::HtmlSelectElement>() else {
                                        return;
                                    };
                                    if let Ok(val) = target.value().parse::<f64>() {
                                        speed.set(val);
                                    }
                                }
                                on:mouseenter=move |e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("border-color", "#3a3f5c").ok();
                                        el.style().set_property("color", "#e2e0d8").ok();
                                    }
                                }
                                on:mouseleave=move |e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("border-color", "#282c3e").ok();
                                        el.style().set_property("color", "#9a9590").ok();
                                    }
                                }
                            >
                                {speed_options.iter().map(|&s| {
                                    let label = format!("{}x", s as i32);
                                    let val = format!("{s}");
                                    view! {
                                        <option value=val>{label}</option>
                                    }
                                }).collect::<Vec<_>>()}
                            </select>
                        }.into_any()
                    }
                }}

                // Current time display + history badge / close button — pushed to end
                <div
                    style:display=move || if is_mobile.get() { "flex" } else { "none" }
                    style="margin-left: auto; align-items: center; gap: 8px; flex-shrink: 0;"
                >
                    <span style="color: #e2e0d8; font-size: 0.7rem; text-align: center; font-variant-numeric: tabular-nums;">
                        {move || timestamp.get().map(format_history_timestamp).unwrap_or_default()}
                    </span>
                    // Desktop: "History" badge
                    <span
                        style:display=move || if is_mobile.get() { "none" } else { "inline" }
                        style="border: 1px solid #3a3f5c; border-radius: 4px; padding: 5px 12px; color: #9a9590; font-family: 'JetBrains Mono', monospace; font-size: 0.68rem; font-weight: 700; letter-spacing: 0.05em;"
                    >
                        "History"
                    </span>
                    // Mobile: close/exit history button
                    <button
                        style:display=move || if is_mobile.get() { "flex" } else { "none" }
                        style="min-width: 44px; min-height: 44px; background: #1a1d2a; border: 1px solid #3a3f5c; border-radius: 6px; color: #9a9590; cursor: pointer; align-items: center; justify-content: center; flex-shrink: 0; transition: background 0.15s ease, color 0.15s ease; touch-action: manipulation;"
                        title="Exit history mode"
                        on:click=move |_| {
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
                        }
                    >
                        // X icon
                        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="16" height="16">
                            <path d="M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z" />
                        </svg>
                    </button>
                </div>
            </div>

            // --- Timeline focus + detail rails ---
            <div
                style:flex=move || if is_mobile.get() { "0 0 auto" } else { "1 1 auto" }
                style="display: flex; flex-direction: column; justify-content: center; gap: 5px; width: 100%; min-width: 0;"
            >
                <div style="display: flex; align-items: center; gap: 8px; width: 100%; min-width: 0;">
                    // Divider: speed | timeline (desktop only)
                    <div
                        style:display=move || if is_mobile.get() { "none" } else { "block" }
                        style="width: 1px; height: 24px; background: #282c3e; margin-right: 2px; flex-shrink: 0;"
                    />

                    <span
                        style:display=move || if is_mobile.get() { "none" } else { "inline" }
                        style="color: #5a5860; flex-shrink: 0; font-size: 0.61rem;"
                    >
                        {move || bounds.get().map(|(earliest, _)| format_history_timestamp(earliest)).unwrap_or_default()}
                    </span>

                    <div class="timeline-overview-wrap">
                        <div class="timeline-focus-band" style=move || focus_band_style(focus_window.get(), bounds.get()) />
                        <input
                            node_ref=overview_ref
                            type="range"
                            class="timeline-slider timeline-overview-slider"
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
                        />
                    </div>

                    <span
                        style:display=move || if is_mobile.get() { "none" } else { "inline" }
                        style="color: #5a5860; flex-shrink: 0; font-size: 0.61rem;"
                    >
                        {move || bounds.get().map(|(_, latest)| format_history_timestamp(latest)).unwrap_or_default()}
                    </span>

                    <div style="display: inline-flex; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; overflow: hidden; flex-shrink: 0;">
                        {HISTORY_FOCUS_SPANS.iter().enumerate().map(|(idx, &(span, label, is_wide))| {
                            view! {
                                <button
                                    type="button"
                                    title=format!("Set timeline zoom to {label}")
                                    style=move || {
                                        let active = focus_span.get() == span;
                                        format!(
                                            "min-width: {}; height: 22px; padding: 0 7px; border: none; border-left: 1px solid {}; background: {}; color: {}; font-family: 'JetBrains Mono', monospace; font-size: 0.61rem; cursor: pointer; touch-action: manipulation;",
                                            if is_wide { "34px" } else { "30px" },
                                            if idx == 0 { "transparent" } else { "#282c3e" },
                                            if active { "rgba(245,197,66,0.14)" } else { "transparent" },
                                            if active { "#f5c542" } else { "#7c829e" },
                                        )
                                    }
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

                <div style="display: flex; align-items: center; gap: 0; width: 100%; min-width: 0;">
                    <span style="color: #5a5860; flex-shrink: 0; font-size: 0.65rem;">
                        {move || {
                            focus_window
                                .get()
                                .or_else(|| bounds.get())
                                .map(|(start, _)| format_history_timestamp(start))
                                .unwrap_or_default()
                        }}
                    </span>

                    <input
                        node_ref=slider_ref
                        type="range"
                        class="timeline-slider"
                        style="flex: 1; margin: 0 8px;"
                        min=move || {
                            focus_window
                                .get()
                                .or_else(|| bounds.get())
                                .map(|(start, _)| start.to_string())
                                .unwrap_or_else(|| "0".to_string())
                        }
                        max=move || {
                            focus_window
                                .get()
                                .or_else(|| bounds.get())
                                .map(|(_, end)| end.to_string())
                                .unwrap_or_else(|| "0".to_string())
                        }
                        step="1"
                        value=timestamp.get_untracked().unwrap_or(0).to_string()
                        on:input=on_range_input
                        on:change=on_range_change
                    />

                    <span style="color: #5a5860; flex-shrink: 0; font-size: 0.65rem;">
                        {move || {
                            focus_window
                                .get()
                                .or_else(|| bounds.get())
                                .map(|(_, end)| format_history_timestamp(end))
                                .unwrap_or_default()
                        }}
                    </span>

                    // Desktop-only: dividers + current time + mode indicator (moved to row 1 on mobile)
                    <div
                        style:display=move || if is_mobile.get() { "none" } else { "flex" }
                        style="align-items: center; gap: 0; flex-shrink: 0;"
                    >
                        <div style="width: 1px; height: 24px; background: #282c3e; margin: 0 10px; flex-shrink: 0;" />
                        <span style="color: #e2e0d8; flex-shrink: 0; min-width: 140px; text-align: center; font-size: 0.7rem;">
                            {move || timestamp.get().map(format_history_timestamp).unwrap_or_default()}
                        </span>
                        <div style="width: 1px; height: 24px; background: #282c3e; margin: 0 10px; flex-shrink: 0;" />
                        <span
                            style="border: 1px solid #3a3f5c; border-radius: 4px; padding: 5px 12px; color: #9a9590; font-family: 'JetBrains Mono', monospace; font-size: 0.68rem; font-weight: 700; flex-shrink: 0; letter-spacing: 0.05em;"
                        >
                            "History"
                        </span>
                    </div>
                </div>
            </div>

        </div>
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_history_timestamp_includes_year() {
        assert_eq!(format_history_timestamp(0), "1970-01-01 00:00");
    }

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
