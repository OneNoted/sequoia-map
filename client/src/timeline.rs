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

fn format_history_timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
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
    let slider_ref_sync = slider_ref.clone();
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

    Effect::new(move || {
        let ts = timestamp.get().unwrap_or(0);
        if !scrub_active.get()
            && let Some(input) = slider_ref_sync.get()
        {
            input.set_value(&ts.to_string());
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
            style:bottom=move || if is_mobile.get() { "94px" } else { "56px" }
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
            style:height=move || if is_mobile.get() { "auto" } else { "52px" }
            style:padding=move || if is_mobile.get() { "8px 12px" } else { "0 16px" }
            style:gap=move || if is_mobile.get() { "6px" } else { "0" }
            style="position: absolute; bottom: 0; left: 0; z-index: 25; background: #13161f; border-top: 1px solid rgba(245,197,66,0.15); align-items: center; font-family: 'JetBrains Mono', monospace; font-size: 0.72rem; transition: right 0.3s cubic-bezier(0.4, 0, 0.2, 1);"
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

            // --- Row 2: Slider (always full-width, but on desktop shares row 1) ---
            <div
                style:flex=move || if is_mobile.get() { "0 0 auto" } else { "1 1 auto" }
                style="display: flex; align-items: center; gap: 0; width: 100%; min-width: 0;"
            >
                // Divider: speed | timeline (desktop only — mobile uses row separation)
                <div
                    style:display=move || if is_mobile.get() { "none" } else { "block" }
                    style="width: 1px; height: 24px; background: #282c3e; margin: 0 10px; flex-shrink: 0;"
                />

                // Left timestamp label
                <span style="color: #5a5860; flex-shrink: 0; font-size: 0.65rem;">
                    {move || bounds.get().map(|(earliest, _)| format_history_timestamp(earliest)).unwrap_or_default()}
                </span>

                // Timeline range slider
                <input
                    node_ref=slider_ref
                    type="range"
                    class="timeline-slider"
                    style="flex: 1; margin: 0 8px;"
                    min=move || bounds.get().map(|(e, _)| e.to_string()).unwrap_or_else(|| "0".to_string())
                    max=move || bounds.get().map(|(_, l)| l.to_string()).unwrap_or_else(|| "0".to_string())
                    step="1"
                    value=timestamp.get_untracked().unwrap_or(0).to_string()
                    on:input=on_range_input
                    on:change=on_range_change
                />

                // Right timestamp label
                <span style="color: #5a5860; flex-shrink: 0; font-size: 0.65rem;">
                    {move || bounds.get().map(|(_, latest)| format_history_timestamp(latest)).unwrap_or_default()}
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
}
