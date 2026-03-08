use leptos::prelude::*;
use std::collections::HashMap;
use wasm_bindgen::JsCast;

use sequoia_shared::history::HistoryHeatMeta;
use sequoia_shared::{
    DataProvenance, Resources, TreasuryLevel, passive_sr_per_5s, passive_sr_per_hour,
};

use crate::app::{
    AbbreviateNames, AutoSrScalarEnabled, BoldConnections, CONNECTION_OPACITY_SCALE_MAX,
    CONNECTION_OPACITY_SCALE_MIN, CONNECTION_THICKNESS_SCALE_MAX, CONNECTION_THICKNESS_SCALE_MIN,
    ConnectionOpacityScale, ConnectionThicknessScale, CurrentMode,
    DEFAULT_CONNECTION_OPACITY_SCALE, DEFAULT_CONNECTION_THICKNESS_SCALE,
    DEFAULT_LABEL_SCALE_GROUP, DEFAULT_LABEL_SCALE_MASTER, DEFAULT_LABEL_SCALE_STATIC_NAME,
    DEFAULT_LABEL_SCALE_STATIC_TAG, DetailReturnGuild, GuildColorStore, GuildOnlineData,
    HeatEntriesByTerritory, HeatFallbackApplied, HeatHistoryBasis, HeatHistoryBasisSetting,
    HeatLiveSource, HeatLiveSourceSetting, HeatMetaState, HeatModeEnabled, HeatSelectedSeasonId,
    HeatWindowLabel, HistoryAvailable, HistoryBoundsSignal, HistoryBufferModeActive,
    HistoryBufferedUpdates, HistoryFetchNonce, HistorySeasonLeaderboard, HistorySeasonScalarSample,
    HistoryTimestamp, IsMobile, LABEL_SCALE_GROUP_MAX, LABEL_SCALE_GROUP_MIN,
    LABEL_SCALE_MASTER_MAX, LABEL_SCALE_MASTER_MIN, LabelScaleDynamic, LabelScaleIcons,
    LabelScaleMaster, LabelScaleStatic, LabelScaleStaticName, LastLiveSeq, LeaderboardSortBySr,
    LiveHandoffResyncCount, LiveSeasonScalarSample, ManualSrScalar, MapMode, NameColor,
    NameColorSetting, NeedsLiveResync, PlaybackActive, ReadableFont, ResetSettingsTrigger,
    ResourceHighlight, Selected, SelectedGuild, ShowCompoundMapTime, ShowCountdown,
    ShowGranularMapTime, ShowLeaderboardOnline, ShowLeaderboardSrGain, ShowLeaderboardSrValue,
    ShowLeaderboardTerritoryCount, ShowMinimap, ShowNames, ShowResourceIcons, ShowSettings,
    ShowTerritoryOrnaments, SidebarIndex, SidebarItems, SidebarOpen, SidebarTransient,
    TagColorSetting, TerritoryGeometryStore, ThickCooldownBorders, canvas_dimensions,
    clamp_connection_opacity_scale, clamp_connection_thickness_scale, clamp_label_scale_group,
    clamp_label_scale_master,
};
use crate::colors::rgba_css;
use crate::history;
use crate::icons;
use crate::season_scalar::{ScalarSource, effective_scalar};
use crate::sse::ConnectionStatus;
use crate::territory::ClientTerritoryMap;
use crate::tower::TowerCalculator;
use crate::viewport::Viewport;
use crate::{IRIS_RELEASES_URL, SEQUOIA_WEBSITE_URL};

/// Build list of (label, formatted_value, icon_name) for non-zero resources.
fn build_resource_items(res: &Resources) -> Vec<(&'static str, String, &'static str)> {
    let mut items = Vec::new();
    if res.emeralds > 0 {
        items.push(("Emeralds", format_resource(res.emeralds), "emerald"));
    }
    if res.ore > 0 {
        items.push(("Ore", format_resource(res.ore), "ore"));
    }
    if res.crops > 0 {
        items.push(("Crops", format_resource(res.crops), "crops"));
    }
    if res.fish > 0 {
        items.push(("Fish", format_resource(res.fish), "fish"));
    }
    if res.wood > 0 {
        items.push(("Wood", format_resource(res.wood), "wood"));
    }
    items
}

fn format_resource(val: i32) -> String {
    if val >= 1000 {
        format!("{:.1}k", val as f64 / 1000.0)
    } else {
        format!("{}", val)
    }
}

fn format_sr_rate(val: f64) -> String {
    if val >= 1000.0 {
        format!("{:.1}k", val / 1000.0)
    } else if val >= 100.0 {
        format!("{:.0}", val)
    } else if val >= 10.0 {
        format!("{:.1}", val)
    } else {
        format!("{:.2}", val)
    }
}

fn format_sr_value(val: i64) -> String {
    if val >= 1_000_000 {
        format!("{:.1}M", val as f64 / 1_000_000.0)
    } else if val >= 1_000 {
        format!("{:.1}k", val as f64 / 1_000.0)
    } else {
        format!("{val}")
    }
}

/// Format an RFC3339 timestamp into a human-readable relative time.
fn format_relative_time(rfc3339: &str, reference_secs: i64) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let secs = reference_secs - dt.timestamp();
    if secs <= 0 {
        return "just now".to_string();
    }
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = secs / 3600;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = secs / 86_400;
    if days < 7 {
        return format!("{}d ago", days);
    }
    if days < 30 {
        let weeks = days / 7;
        return format!("{}w ago", weeks);
    }
    // Fallback to short date
    dt.format("%b %d, %Y").to_string()
}

fn format_confidence(confidence: f32) -> String {
    format!("{:.0}%", (confidence.clamp(0.0, 1.0) * 100.0))
}

fn format_age_seconds(age_secs: i64) -> String {
    let secs = age_secs.max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let mins = secs / 60;
        return format!("{mins}m {}s", secs % 60);
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    format!("{hours}h {mins}m")
}

fn visibility_label(provenance: &DataProvenance) -> &'static str {
    match provenance.visibility {
        sequoia_shared::VisibilityClass::Public => "Public",
        sequoia_shared::VisibilityClass::GuildOptIn => "Guild Opt-In",
    }
}

fn iris_source_display_label(source: &str) -> Option<&'static str> {
    if source.eq_ignore_ascii_case("iris") || source.eq_ignore_ascii_case("fabric_reporter") {
        Some("iris")
    } else {
        None
    }
}

/// Sidebar with search, leaderboard, detail panel, and stats.
#[component]
pub fn Sidebar() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let Selected(selected) = expect_context();
    let SelectedGuild(selected_guild) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();
    let ShowSettings(show_settings) = expect_context();

    // Scroll focused item into view when index changes
    Effect::new(move || {
        if !sidebar_open.get() {
            return;
        }
        let idx = sidebar_index.get();
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(doc) = window.document() else {
            return;
        };
        let Ok(Some(scroll_el)) = doc.query_selector("[data-sidebar-scroll]") else {
            return;
        };
        let Ok(scroll_el) = scroll_el.dyn_into::<web_sys::HtmlElement>() else {
            return;
        };
        let Ok(Some(item_el)) = scroll_el.query_selector(&format!("[data-sidebar-idx='{}']", idx))
        else {
            return;
        };
        let Ok(item_el) = item_el.dyn_into::<web_sys::HtmlElement>() else {
            return;
        };

        // Keep keyboard-focused rows visible by adjusting only the sidebar's
        // internal vertical scroll position (never root page scroll).
        let scroll_rect = scroll_el.get_bounding_client_rect();
        let item_rect = item_el.get_bounding_client_rect();
        let current_top = scroll_el.scroll_top();
        if item_rect.top() < scroll_rect.top() {
            let delta = (item_rect.top() - scroll_rect.top()).floor() as i32;
            scroll_el.set_scroll_top(current_top + delta);
        } else if item_rect.bottom() > scroll_rect.bottom() {
            let delta = (item_rect.bottom() - scroll_rect.bottom()).ceil() as i32;
            scroll_el.set_scroll_top(current_top + delta);
        }
    });

    view! {
        <div
            class="sidebar-inner"
            class:sidebar-animate=move || sidebar_open.get()
            style:display=move || if sidebar_open.get() { "flex" } else { "none" }
            style="width: 100%; min-width: 100%; height: 100%; background: #13161f; border-left: 1px solid #282c3e; display: flex; flex-direction: column; z-index: 10; box-shadow: -4px 0 20px rgba(0,0,0,0.4), inset 1px 0 0 rgba(168,85,247,0.04);"
        >
            <SidebarHeader />
            <SearchBar />
            <div data-sidebar-scroll="" class="scrollbar-thin" style="flex: 1; overflow-y: auto;">
                {move || {
                    if show_settings.get() {
                        sidebar_items.set(Vec::new());
                        sidebar_index.set(0);
                        view! { <SettingsPanel /> }.into_any()
                    } else if selected_guild.get().is_some() {
                        sidebar_items.set(Vec::new());
                        sidebar_index.set(0);
                        view! { <GuildPanel /> }.into_any()
                    } else if selected.get().is_some() {
                        sidebar_items.set(Vec::new());
                        sidebar_index.set(0);
                        view! { <DetailPanel /> }.into_any()
                    } else {
                        let query = search_query.get();
                        if query.is_empty() {
                            view! { <LeaderboardPanel /> }.into_any()
                        } else {
                            view! { <SearchResults /> }.into_any()
                        }
                    }
                }}
            </div>
            <StatsBar />
        </div>
    }
}

#[component]
fn SidebarHeader() -> impl IntoView {
    let IsMobile(is_mobile) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let tick: RwSignal<i64> = expect_context();

    #[derive(Clone, PartialEq)]
    struct IrisBadgeStatus {
        is_working: bool,
        status_label: String,
        status_reason: String,
        source: String,
        observed_label: String,
        age_label: String,
        reporter_count: u16,
        visibility: String,
        confidence: String,
        tooltip: String,
    }

    let iris_status = Memo::new(move |_| {
        let now = tick.get();
        let map = territories.get();
        let mut latest_status: Option<(i64, String, String, u16, String, String)> = None;

        for ct in map.values() {
            let Some(runtime) = ct.territory.runtime.as_ref() else {
                continue;
            };
            let Some(provenance) = runtime.provenance.as_ref() else {
                continue;
            };
            let source = provenance.source.trim();
            let Some(source_label) = iris_source_display_label(source) else {
                continue;
            };

            let observed_ts = chrono::DateTime::parse_from_rfc3339(&provenance.observed_at)
                .map(|dt| dt.timestamp())
                .unwrap_or(now);
            if latest_status
                .as_ref()
                .is_none_or(|(prev_ts, _, _, _, _, _)| observed_ts > *prev_ts)
            {
                latest_status = Some((
                    observed_ts,
                    source_label.to_string(),
                    provenance.observed_at.clone(),
                    provenance.reporter_count,
                    visibility_label(provenance).to_string(),
                    format_confidence(provenance.confidence),
                ));
            }
        }

        match latest_status {
            Some((observed_ts, source, observed_at, reporter_count, visibility, confidence)) => {
                let age_secs = (now - observed_ts).max(0);
                let is_active = age_secs <= 120;
                let is_working = is_active && reporter_count > 0;
                let status_label = if is_working {
                    "Working".to_string()
                } else {
                    "Not Working".to_string()
                };
                let observed_label = format_relative_time(&observed_at, now);
                let age_label = format_age_seconds(age_secs);
                let status_reason = if is_working {
                    "recent Iris telemetry is arriving".to_string()
                } else if !is_active {
                    format!("latest Iris telemetry is stale ({age_label} old)")
                } else {
                    "no active Iris reporters".to_string()
                };
                IrisBadgeStatus {
                    is_working,
                    status_label: status_label.clone(),
                    status_reason: status_reason.clone(),
                    source: source.clone(),
                    observed_label: observed_label.clone(),
                    age_label: age_label.clone(),
                    reporter_count,
                    visibility: visibility.clone(),
                    confidence: confidence.clone(),
                    tooltip: format!(
                        "Iris: {status_label}\nReason: {status_reason}\nSource: {source}\nObserved: {observed_label}\nAge: {age_label}\nReporters: {reporter_count}\nVisibility: {visibility}\nConfidence: {confidence}\nFreshness window: 120s"
                    ),
                }
            }
            None => {
                let status_label = "Not Working".to_string();
                let status_reason = "no Iris telemetry received yet".to_string();
                IrisBadgeStatus {
                    is_working: false,
                    status_label: status_label.clone(),
                    status_reason: status_reason.clone(),
                    source: "none".to_string(),
                    observed_label: "no observations yet".to_string(),
                    age_label: "n/a".to_string(),
                    reporter_count: 0,
                    visibility: "n/a".to_string(),
                    confidence: "n/a".to_string(),
                    tooltip: format!(
                        "Iris: {status_label}\nReason: {status_reason}\nSource: none\nObserved: no observations yet"
                    ),
                }
            }
        }
    });
    let iris_tooltip_visible = RwSignal::new(false);

    let padding = move || {
        if is_mobile.get() {
            "padding: 10px 16px 8px; border-bottom: 1px solid #282c3e;"
        } else {
            "padding: 20px 24px 16px; border-bottom: 1px solid #282c3e;"
        }
    };
    let divider_margin = move || {
        if is_mobile.get() {
            "margin-top: 6px;"
        } else {
            "margin-top: 12px;"
        }
    };

    view! {
        <div style=padding>
            <div style="display: flex; justify-content: space-between; align-items: flex-start; gap: 12px;">
                <a
                    href=SEQUOIA_WEBSITE_URL
                    target="_blank"
                    rel="noopener noreferrer"
                    title="Open seqwawa.com"
                    style="display: flex; align-items: baseline; gap: 10px; min-width: 0; text-decoration: none;"
                >
                    <div class="text-gold-gradient" style="font-family: 'Silkscreen', monospace; font-size: 1.35rem; font-weight: 700; letter-spacing: 0.18em; text-transform: uppercase; text-shadow: 0 0 16px rgba(245,197,66,0.08);">"SEQUOIA"</div>
                    <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.673rem; color: #3a3f5c; background: #1a1d2a; padding: 1px 6px; border-radius: 3px; border: 1px solid rgba(245,197,66,0.15); letter-spacing: 0.04em;">"v0.1"</div>
                </a>
                <div style="position: relative; flex-shrink: 0;">
                    <a
                        href=IRIS_RELEASES_URL
                        target="_blank"
                        rel="noopener noreferrer"
                        title="Open Iris releases"
                        style=move || {
                            let status = iris_status.get();
                            let (border, bg, fg) = if status.is_working {
                                (
                                    "rgba(var(--accent-live-rgb),0.42)",
                                    "rgba(28, 42, 72, 0.65)",
                                    "var(--accent-live)",
                                )
                            } else {
                                (
                                    "rgba(154,149,144,0.36)",
                                    "rgba(32, 35, 48, 0.62)",
                                    "#9a9590",
                                )
                            };
                            format!(
                                "display: inline-flex; align-items: center; gap: 6px; font-family: 'Silkscreen', monospace; font-size: 0.66rem; letter-spacing: 0.08em; padding: 3px 7px; border-radius: 999px; border: 1px solid {border}; background: {bg}; color: {fg}; cursor: pointer; text-decoration: none;"
                            )
                        }
                        aria-label=move || iris_status.get().tooltip
                        on:mouseenter=move |_| iris_tooltip_visible.set(true)
                        on:mouseleave=move |_| iris_tooltip_visible.set(false)
                    >
                        <span style=move || {
                            let status = iris_status.get();
                            let (dot_bg, dot_glow) = if status.is_working {
                                ("var(--accent-live)", "0 0 6px var(--accent-live)")
                            } else {
                                ("#9a9590", "none")
                            };
                            format!(
                                "width: 7px; height: 7px; border-radius: 50%; background: {dot_bg}; box-shadow: {dot_glow};"
                            )
                        } />
                        <span>"Iris"</span>
                    </a>
                    <div
                        style=move || {
                            let status = iris_status.get();
                            let border = if status.is_working {
                                "rgba(var(--accent-live-rgb),0.42)"
                            } else {
                                "rgba(154,149,144,0.45)"
                            };
                            let display = if iris_tooltip_visible.get() {
                                "block"
                            } else {
                                "none"
                            };
                            format!(
                                "position: absolute; top: calc(100% + 8px); right: 0; min-width: 245px; max-width: 310px; border-radius: 8px; border: 1px solid {border}; background: rgba(15, 18, 30, 0.97); box-shadow: 0 10px 28px rgba(0,0,0,0.35); padding: 9px 10px; z-index: 20; pointer-events: none; display: {display};"
                            )
                        }
                    >
                        <div style=move || {
                            let color = if iris_status.get().is_working {
                                "var(--accent-live)"
                            } else {
                                "#d5d2ce"
                            };
                            format!(
                                "font-family: 'Silkscreen', monospace; font-size: 0.62rem; letter-spacing: 0.09em; text-transform: uppercase; color: {color};"
                            )
                        }>
                            {move || format!("Iris {}", iris_status.get().status_label)}
                        </div>
                        <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.67rem; color: #c5c2bc; margin-top: 4px;">
                            {move || iris_status.get().status_reason}
                        </div>
                        <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.645rem; color: #9a9590; margin-top: 8px; line-height: 1.45;">
                            <div>{move || format!("Observed: {}", iris_status.get().observed_label)}</div>
                            <div>{move || format!("Age: {}", iris_status.get().age_label)}</div>
                            <div>{move || format!("Source: {}", iris_status.get().source)}</div>
                            <div>{move || format!("Reporters: {}", iris_status.get().reporter_count)}</div>
                            <div>{move || format!("Visibility: {}", iris_status.get().visibility)}</div>
                            <div>{move || format!("Confidence: {}", iris_status.get().confidence)}</div>
                            <div>"Freshness window: 120s"</div>
                        </div>
                    </div>
                </div>
            </div>
            <div
                style="font-family: 'Inter', system-ui, sans-serif; font-size: 0.835rem; color: #5a5860; margin-top: 3px; letter-spacing: 0.08em;"
                style:display=move || if is_mobile.get() { "none" } else { "block" }
            >"Wynncraft Territories"</div>
            // Gradient line divider
            <div class="divider-gold" style=divider_margin />
        </div>
    }
}

#[component]
fn SearchBar() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let IsMobile(is_mobile) = expect_context();

    let on_input = move |e: leptos::ev::Event| {
        let Some(target) = e.target() else {
            return;
        };
        let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        search_query.set(input.value());
    };

    let outer_padding = move || {
        if is_mobile.get() {
            "padding: 8px 12px; border-bottom: 1px solid #282c3e;"
        } else {
            "padding: 12px 24px; border-bottom: 1px solid #282c3e;"
        }
    };

    view! {
        <div style=outer_padding>
            <div style="position: relative;">
                // Search icon (inline SVG magnifying glass)
                <div style="position: absolute; left: 12px; top: 50%; transform: translateY(-50%); pointer-events: none; color: #5a5860; width: 14px; height: 14px;">
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="14" height="14">
                        <path fill-rule="evenodd" d="M9 3.5a5.5 5.5 0 100 11 5.5 5.5 0 000-11zM2 9a7 7 0 1112.452 4.391l3.328 3.329a.75.75 0 11-1.06 1.06l-3.329-3.328A7 7 0 012 9z" clip-rule="evenodd" />
                    </svg>
                </div>
                <input
                    data-search-input=""
                    class="focus-ring"
                    style="width: 100%; padding: 10px 14px 10px 34px; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 6px; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif; font-size: 1.044rem; outline: none; transition: border-color 0.2s ease, box-shadow 0.3s ease;"
                    type="text"
                    placeholder="Search territories or guilds..."
                    prop:value=move || search_query.get()
                    on:input=on_input
                    on:focus=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "#f5c542").ok();
                            el.style().set_property("box-shadow", "0 0 12px rgba(245,197,66,0.08)").ok();
                        }
                    }
                    on:blur=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "#282c3e").ok();
                            el.style().set_property("box-shadow", "none").ok();
                        }
                    }
                />
                // Keyboard hint — hidden on mobile (irrelevant on touch)
                <div
                    style="position: absolute; right: 10px; top: 50%; transform: translateY(-50%); font-family: 'JetBrains Mono', monospace; font-size: 0.719rem; color: #3a3f5c; background: #13161f; padding: 1px 5px; border-radius: 3px; border: 1px solid #282c3e; pointer-events: none;"
                    style:display=move || if is_mobile.get() { "none" } else { "block" }
                >"/"</div>
            </div>
        </div>
    }
}

#[component]
fn SettingsPanel() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let AbbreviateNames(abbreviate_names) = expect_context();
    let show_connections: RwSignal<bool> = expect_context();
    let ShowCountdown(show_countdown) = expect_context();
    let ShowGranularMapTime(show_granular_map_time) = expect_context();
    let ShowCompoundMapTime(show_compound_map_time) = expect_context();
    let ShowNames(show_names) = expect_context();
    let ThickCooldownBorders(thick_cooldown_borders) = expect_context();
    let BoldConnections(bold_connections) = expect_context();
    let ConnectionOpacityScale(connection_opacity_scale) = expect_context();
    let ConnectionThicknessScale(connection_thickness_scale) = expect_context();
    let ResourceHighlight(resource_highlight) = expect_context();
    let ShowResourceIcons(show_resource_icons) = expect_context();
    let ShowTerritoryOrnaments(show_territory_ornaments) = expect_context();
    let ManualSrScalar(manual_sr_scalar) = expect_context();
    let AutoSrScalarEnabled(auto_sr_scalar_enabled) = expect_context();
    let ShowLeaderboardSrGain(show_leaderboard_sr_gain) = expect_context();
    let ShowLeaderboardSrValue(show_leaderboard_sr_value) = expect_context();
    let ShowLeaderboardTerritoryCount(show_leaderboard_territory_count) = expect_context();
    let ShowLeaderboardOnline(show_leaderboard_online) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatLiveSourceSetting(heat_live_source) = expect_context();
    let HeatHistoryBasisSetting(heat_history_basis) = expect_context();
    let HeatSelectedSeasonId(heat_selected_season_id) = expect_context();
    let HeatMetaState(heat_meta) = expect_context();
    let HeatFallbackApplied(heat_fallback_applied) = expect_context();
    let HeatWindowLabel(heat_window_label) = expect_context();
    let CurrentMode(mode) = expect_context();
    let ReadableFont(readable_font) = expect_context();
    let NameColorSetting(name_color) = expect_context();
    let TagColorSetting(tag_color) = expect_context();
    let ShowMinimap(show_minimap) = expect_context();
    let LabelScaleMaster(label_scale_master) = expect_context();
    let LabelScaleStatic(label_scale_static_tag) = expect_context();
    let LabelScaleStaticName(label_scale_static_name) = expect_context();
    let LabelScaleDynamic(label_scale_dynamic) = expect_context();
    let LabelScaleIcons(label_scale_icons) = expect_context();
    let territory_count = Memo::new(move |_| territories.get().len());

    view! {
        <div style="border-bottom: 1px solid #282c3e;">
            <div style="padding: 12px 14px 8px; display: flex; align-items: center; gap: 10px;">
                <button
                    style="width: 30px; height: 30px; border-radius: 999px; border: 1px solid #282c3e; background: #1a1d2a; color: #9a9590; display: flex; align-items: center; justify-content: center; cursor: pointer; transition: border-color 0.15s, background 0.15s, color 0.15s; font-family: 'JetBrains Mono', monospace; font-size: 0.92rem; line-height: 1;"
                    title="Back"
                    on:click=move |_| show_settings.set(false)
                    on:mouseenter=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("color", "#f5c542").ok();
                            el.style().set_property("border-color", "rgba(245,197,66,0.35)").ok();
                            el.style().set_property("background", "#13161f").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("color", "#9a9590").ok();
                            el.style().set_property("border-color", "#282c3e").ok();
                            el.style().set_property("background", "#1a1d2a").ok();
                        }
                    }
                >
                    "\u{2039}"
                </button>
                <div style="font-family: 'Silkscreen', monospace; font-size: 0.986rem; text-transform: uppercase; letter-spacing: 0.14em; color: #5a5860;">
                    <span style="color: #f5c542; margin-right: 6px; font-size: 0.812rem;">{"\u{2699}"}</span>"Settings"
                </div>
            </div>
            <div style="padding: 0 12px 12px;">
                <SettingsSectionHeader title="Labels" />
                <SettingsToggleRow label="Territory Names" shortcut="N" active=show_names />
                <SettingsToggleRow label="Abbreviate Names" shortcut="A" active=abbreviate_names />
                <SettingsToggleRow label="Readable Font" shortcut="F" active=readable_font />
                <SettingsNameColorRow color=name_color />
                <SettingsNameColorRow color=tag_color label="Tag Color" />

                <SettingsSectionHeader title="Label Scale" />
                <SettingsScaleRow
                    label="Master"
                    value=label_scale_master
                    min=LABEL_SCALE_MASTER_MIN
                    max=LABEL_SCALE_MASTER_MAX
                    step=0.05
                    clamp=clamp_label_scale_master
                />
                <SettingsScaleRow
                    label="Guild Tag"
                    value=label_scale_static_tag
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                />
                <SettingsScaleRow
                    label="Territory Name"
                    value=label_scale_static_name
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                />
                <SettingsScaleRow
                    label="Timers & Cooldowns"
                    value=label_scale_dynamic
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                />
                <SettingsScaleRow
                    label="Resource Icons"
                    value=label_scale_icons
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                />
                <SettingsScaleResetRow
                    master=label_scale_master
                    static_tag=label_scale_static_tag
                    static_name=label_scale_static_name
                    dynamic=label_scale_dynamic
                    icons=label_scale_icons
                />

                <SettingsSectionHeader title="Timing" />
                <SettingsToggleRow label="Countdown Timer" shortcut="T" active=show_countdown />
                <SettingsToggleRow label="Granular Map Time" shortcut="" active=show_granular_map_time />
                <SettingsToggleRow label="Compound Map Time" shortcut="" active=show_compound_map_time />
                <SettingsToggleRow label="Thick Cooldown Borders" shortcut="" active=thick_cooldown_borders />

                <SettingsSectionHeader title="Map" />
                <SettingsToggleRow label="Connection Lines" shortcut="C" active=show_connections />
                <SettingsToggleRow label="Bold Connections" shortcut="B" active=bold_connections />
                <SettingsScaleRow
                    label="Line Opacity"
                    value=connection_opacity_scale
                    min=CONNECTION_OPACITY_SCALE_MIN
                    max=CONNECTION_OPACITY_SCALE_MAX
                    step=0.05
                    clamp=clamp_connection_opacity_scale
                />
                <SettingsScaleRow
                    label="Line Thickness"
                    value=connection_thickness_scale
                    min=CONNECTION_THICKNESS_SCALE_MIN
                    max=CONNECTION_THICKNESS_SCALE_MAX
                    step=0.05
                    clamp=clamp_connection_thickness_scale
                />
                <SettingsConnectionScaleResetRow
                    opacity=connection_opacity_scale
                    thickness=connection_thickness_scale
                />
                <SettingsToggleRow label="Resource Highlight" shortcut="P" active=resource_highlight />
                <SettingsToggleRow label="Resource Icons" shortcut="" active=show_resource_icons />
                <SettingsToggleRow label="Territory Ornaments" shortcut="" active=show_territory_ornaments />
                <SettingsToggleRow label="Minimap" shortcut="M" active=show_minimap />
                <SettingsToggleRow label="Heat Map" shortcut="" active=heat_mode_enabled />
                <div style="display: flex; align-items: center; justify-content: space-between; padding: 9px 10px;">
                    <span style="font-size: 1.021rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif;">"Territories"</span>
                    <span style="font-size: 0.858rem; color: #9a9590; font-family: 'JetBrains Mono', monospace;">
                        {move || territory_count.get()}
                    </span>
                </div>
                <Show when=move || heat_mode_enabled.get()>
                    <div style="padding: 4px 10px 8px; border-top: 1px solid rgba(40,44,62,0.5); margin-top: 4px;">
                        <SettingsHeatSourceRow
                            mode=mode
                            live_source=heat_live_source
                            history_basis=heat_history_basis
                        />
                        <Show when=move || {
                            if mode.get() == MapMode::History {
                                heat_history_basis.get() == HeatHistoryBasis::SeasonCumulative
                            } else {
                                heat_live_source.get() == HeatLiveSource::Season
                            }
                        }>
                            <SettingsHeatSeasonRow season_id=heat_selected_season_id meta=heat_meta />
                        </Show>
                        <Show when=move || heat_fallback_applied.get()>
                            <div style="font-size: 0.766rem; color: #f5c542; font-family: 'JetBrains Mono', monospace; margin-top: 6px;">
                                "Season data unavailable, using last 60d fallback."
                            </div>
                        </Show>
                        <div style="font-size: 0.719rem; color: #6f748f; font-family: 'JetBrains Mono', monospace; margin-top: 5px;">
                            {move || heat_window_label.get()}
                        </div>
                    </div>
                </Show>

                <SettingsSectionHeader title="Season Rating" />
                <SettingsScalarRow scalar=manual_sr_scalar />
                <SettingsToggleRow label="Auto Scalar Estimate" shortcut="" active=auto_sr_scalar_enabled />

                <SettingsSectionHeader title="Leaderboard" />
                <SettingsToggleRow label="Territory Count" shortcut="" active=show_leaderboard_territory_count />
                <SettingsToggleRow label="Online Count" shortcut="" active=show_leaderboard_online />
                <SettingsToggleRow label="SR Gain" shortcut="" active=show_leaderboard_sr_gain />
                <SettingsToggleRow label="SR Value" shortcut="" active=show_leaderboard_sr_value />
            </div>
        </div>
    }
}

const NAME_COLOR_OPTIONS: &[(NameColor, &str, &str)] = &[
    (NameColor::White, "White", "#dcdad2"),
    (NameColor::Guild, "Guild", "#a88cc8"), // representative purple for the swatch
    (NameColor::Gold, "Gold", "#f5c542"),
    (NameColor::Copper, "Copper", "#b56727"),
    (NameColor::Muted, "Muted", "#787470"),
];

#[component]
fn SettingsNameColorRow(
    color: RwSignal<NameColor>,
    #[prop(default = "Name Color")] label: &'static str,
) -> impl IntoView {
    view! {
        <div style="display: flex; align-items: center; justify-content: space-between; padding: 9px 10px;">
            <span style="font-size: 1.021rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif;">{label}</span>
            <div style="display: flex; gap: 6px; align-items: center;">
                {NAME_COLOR_OPTIONS.iter().map(|&(variant, label, css_color)| {
                    let on_click = move |_| color.set(variant);
                    view! {
                        <span
                            title=label
                            style=move || {
                                let selected = color.get() == variant;
                                format!(
                                    "display: inline-block; width: 14px; height: 14px; border-radius: 50%; background: {}; cursor: pointer; border: 2px solid {}; transition: border-color 0.15s, box-shadow 0.15s;{}",
                                    css_color,
                                    if selected { "#e2e0d8" } else { "#2a2e40" },
                                    if selected { format!(" box-shadow: 0 0 5px {}80;", css_color) } else { String::new() },
                                )
                            }
                            on:click=on_click
                        />
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

#[component]
fn SettingsSectionHeader(title: &'static str) -> impl IntoView {
    view! {
        <div
            style="padding: 10px 10px 5px; margin-top: 4px; font-family: 'JetBrains Mono', monospace; font-size: 0.719rem; text-transform: uppercase; letter-spacing: 0.12em; color: #5a5860;"
        >
            {title}
        </div>
    }
}

#[component]
fn SettingsScalarRow(scalar: RwSignal<f64>) -> impl IntoView {
    let on_input = move |e: leptos::ev::Event| {
        let Some(target) = e.target() else {
            return;
        };
        let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        if let Ok(parsed) = input.value().trim().parse::<f64>() {
            scalar.set(crate::season_scalar::clamp_manual_scalar(parsed));
        }
    };

    view! {
        <div
            style="display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 9px 10px; border-radius: 4px;"
        >
            <span style="font-size: 1.021rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif;">
                "Manual Scalar"
            </span>
            <input
                type="number"
                min="0.05"
                max="20"
                step="0.05"
                prop:value=move || format!("{:.2}", scalar.get())
                on:input=on_input
                style="width: 90px; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.835rem; padding: 4px 6px; outline: none;"
            />
        </div>
    }
}

#[component]
fn SettingsScaleRow(
    label: &'static str,
    value: RwSignal<f64>,
    min: f64,
    max: f64,
    step: f64,
    clamp: fn(f64) -> f64,
) -> impl IntoView {
    let slider_ref = NodeRef::<leptos::html::Input>::new();
    let slider_ref_sync = slider_ref.clone();
    let local_value: RwSignal<f64> = RwSignal::new(clamp(value.get_untracked()));
    let dragging: RwSignal<bool> = RwSignal::new(false);

    Effect::new(move || {
        let external = clamp(value.get());
        if !dragging.get() {
            local_value.set(external);
            if let Some(input) = slider_ref_sync.get() {
                input.set_value(&format!("{external:.2}"));
            }
        }
    });

    let on_input = {
        move |e: leptos::ev::Event| {
            let Some(target) = e.target() else {
                return;
            };
            let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                return;
            };
            if let Ok(parsed) = input.value().trim().parse::<f64>() {
                let clamped = clamp(parsed);
                dragging.set(true);
                local_value.set(clamped);
                value.set(clamped);
            }
        }
    };

    let on_change = {
        move |e: leptos::ev::Event| {
            if let Some(target) = e.target()
                && let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>()
                && let Ok(parsed) = input.value().trim().parse::<f64>()
            {
                let clamped = clamp(parsed);
                local_value.set(clamped);
                value.set(clamped);
            }
            dragging.set(false);
        }
    };

    view! {
        <div style="display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: 4px;">
            <span style="min-width: 124px; font-size: 0.951rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif;">
                {label}
            </span>
            <input
                node_ref=slider_ref
                type="range"
                class="timeline-slider"
                min=min
                max=max
                step=step
                value=format!("{:.2}", local_value.get_untracked())
                on:input=on_input
                on:change=on_change
                style="flex: 1; margin: 0; accent-color: #f5c542;"
            />
            <span
                style="width: 42px; text-align: right; font-family: 'JetBrains Mono', monospace; font-size: 0.766rem; color: #9a9590;"
            >
                {move || format!("{:.2}", value.get())}
            </span>
        </div>
    }
}

#[component]
fn SettingsScaleResetRow(
    master: RwSignal<f64>,
    static_tag: RwSignal<f64>,
    static_name: RwSignal<f64>,
    dynamic: RwSignal<f64>,
    icons: RwSignal<f64>,
) -> impl IntoView {
    let on_reset = move |_| {
        master.set(DEFAULT_LABEL_SCALE_MASTER);
        static_tag.set(DEFAULT_LABEL_SCALE_STATIC_TAG);
        static_name.set(DEFAULT_LABEL_SCALE_STATIC_NAME);
        dynamic.set(DEFAULT_LABEL_SCALE_GROUP);
        icons.set(DEFAULT_LABEL_SCALE_GROUP);
    };

    view! {
        <div style="display: flex; justify-content: flex-end; padding: 2px 10px 6px;">
            <button
                on:click=on_reset
                style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #9a9590; font-family: 'JetBrains Mono', monospace; font-size: 0.719rem; padding: 3px 8px; cursor: pointer;"
            >
                "Reset"
            </button>
        </div>
    }
}

#[component]
fn SettingsConnectionScaleResetRow(
    opacity: RwSignal<f64>,
    thickness: RwSignal<f64>,
) -> impl IntoView {
    let on_reset = move |_| {
        opacity.set(DEFAULT_CONNECTION_OPACITY_SCALE);
        thickness.set(DEFAULT_CONNECTION_THICKNESS_SCALE);
    };

    view! {
        <div style="display: flex; justify-content: flex-end; padding: 2px 10px 6px;">
            <button
                on:click=on_reset
                style="background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #9a9590; font-family: 'JetBrains Mono', monospace; font-size: 0.719rem; padding: 3px 8px; cursor: pointer;"
            >
                "Reset"
            </button>
        </div>
    }
}

#[component]
fn SettingsToggleRow(
    label: &'static str,
    shortcut: &'static str,
    active: RwSignal<bool>,
) -> impl IntoView {
    let on_click = move |_| {
        active.update(|v| *v = !*v);
    };

    view! {
        <div
            style="display: flex; align-items: center; justify-content: space-between; padding: 9px 10px; border-radius: 4px; cursor: pointer; transition: background 0.15s;"
            on:click=on_click
            on:mouseenter=|e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("background", "#232738").ok();
                }
            }
            on:mouseleave=|e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("background", "transparent").ok();
                }
            }
        >
            <div style="display: flex; align-items: center; gap: 8px;">
                <span style="font-size: 1.021rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif;">{label}</span>
                {(!shortcut.is_empty()).then(|| view! {
                    <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.673rem; color: #3a3f5c; background: #1a1d2a; padding: 1px 5px; border-radius: 3px; border: 1px solid #282c3e;">{shortcut}</span>
                })}
            </div>
            <span style=move || {
                if active.get() {
                    "display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #50c878; box-shadow: 0 0 5px rgba(80,200,120,0.4); flex-shrink: 0;"
                } else {
                    "display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #3a3f5c; flex-shrink: 0;"
                }
            } />
        </div>
    }
}

#[component]
fn SettingsHeatSourceRow(
    mode: RwSignal<MapMode>,
    live_source: RwSignal<HeatLiveSource>,
    history_basis: RwSignal<HeatHistoryBasis>,
) -> impl IntoView {
    let is_history = move || mode.get() == MapMode::History;
    view! {
        <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; padding-top: 6px;">
            <span style="font-size: 0.928rem; color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">
                {move || if is_history() { "History Basis" } else { "Live Source" }}
            </span>
            <div style="display: inline-flex; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; overflow: hidden;">
                <button
                    style=move || {
                        let active = if is_history() {
                            history_basis.get() == HeatHistoryBasis::SeasonCumulative
                        } else {
                            live_source.get() == HeatLiveSource::Season
                        };
                        format!(
                            "padding: 4px 8px; border: none; background: {}; color: {}; font-family: 'JetBrains Mono', monospace; font-size: 0.766rem; cursor: pointer;",
                            if active { "rgba(245,197,66,0.12)" } else { "transparent" },
                            if active { "#f5c542" } else { "#7c829e" },
                        )
                    }
                    on:click=move |_| {
                        if is_history() {
                            history_basis.set(HeatHistoryBasis::SeasonCumulative);
                        } else {
                            live_source.set(HeatLiveSource::Season);
                        }
                    }
                >
                    "Season"
                </button>
                <button
                    style=move || {
                        let active = if is_history() {
                            history_basis.get() == HeatHistoryBasis::AllTimeCumulative
                        } else {
                            live_source.get() == HeatLiveSource::AllTime
                        };
                        format!(
                            "padding: 4px 8px; border: none; border-left: 1px solid #282c3e; background: {}; color: {}; font-family: 'JetBrains Mono', monospace; font-size: 0.766rem; cursor: pointer;",
                            if active { "rgba(245,197,66,0.12)" } else { "transparent" },
                            if active { "#f5c542" } else { "#7c829e" },
                        )
                    }
                    on:click=move |_| {
                        if is_history() {
                            history_basis.set(HeatHistoryBasis::AllTimeCumulative);
                        } else {
                            live_source.set(HeatLiveSource::AllTime);
                        }
                    }
                >
                    "All-time"
                </button>
            </div>
        </div>
    }
}

#[component]
fn SettingsHeatSeasonRow(
    season_id: RwSignal<Option<i32>>,
    meta: RwSignal<Option<HistoryHeatMeta>>,
) -> impl IntoView {
    let on_change = move |e: leptos::ev::Event| {
        let Some(target) = e.target() else {
            return;
        };
        let Ok(select) = target.dyn_into::<web_sys::HtmlSelectElement>() else {
            return;
        };
        let value = select.value();
        if value == "latest" {
            season_id.set(None);
            return;
        }
        season_id.set(value.parse::<i32>().ok());
    };

    view! {
        <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-top: 6px;">
            <span style="font-size: 0.928rem; color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Season"</span>
            <select
                on:change=on_change
                style="min-width: 120px; background: #1a1d2a; border: 1px solid #282c3e; border-radius: 4px; color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.812rem; padding: 4px 6px; outline: none;"
            >
                <option
                    value="latest"
                    selected=move || season_id.get().is_none()
                >
                    "Latest"
                </option>
                {move || {
                    meta.get()
                        .map(|m| {
                            m.seasons
                                .iter()
                                .map(|season| {
                                    let season_id_value = season.season_id;
                                    let value = season_id_value.to_string();
                                    view! {
                                        <option
                                            value=value.clone()
                                            selected=move || season_id.get() == Some(season_id_value)
                                        >
                                            {format!("Season {season_id_value}")}
                                        </option>
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                }}
            </select>
        </div>
    }
}

#[component]
fn SearchResults() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let Selected(selected) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let viewport: RwSignal<Viewport> = expect_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();

    let filtered = Memo::new(move |_| {
        let query = search_query.get().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        let map = territories.get();
        let mut results: Vec<_> = map
            .iter()
            .filter(|(name, ct)| {
                name.to_lowercase().contains(&query)
                    || ct.territory.guild.name.to_lowercase().contains(&query)
                    || ct.territory.guild.prefix.to_lowercase().contains(&query)
            })
            .map(|(name, ct)| {
                (
                    name.clone(),
                    ct.territory.guild.name.clone(),
                    ct.territory.guild.prefix.clone(),
                    ct.guild_color,
                )
            })
            .collect();

        results.sort_by(|a, b| a.0.cmp(&b.0));
        results.truncate(50);
        results
    });

    // Sync sidebar items for keyboard navigation
    Effect::new(move || {
        let f = filtered.get();
        let items: Vec<String> = f.iter().map(|(name, _, _, _)| name.clone()).collect();
        let prev = sidebar_items.get_untracked();
        if items != prev {
            sidebar_index.set(0);
        }
        sidebar_items.set(items);
    });

    let result_count = Memo::new(move |_| filtered.get().len());

    view! {
        <div style="border-bottom: 1px solid #282c3e;">
            <div style="padding: 14px 24px 8px; display: flex; align-items: baseline; justify-content: space-between;">
                <span style="font-family: 'Silkscreen', monospace; font-size: 0.986rem; text-transform: uppercase; letter-spacing: 0.14em; color: #5a5860;">"Search Results"</span>
                <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.754rem; color: #3a3f5c;">{move || format!("{} found", result_count.get())}</span>
            </div>
            <div style="padding: 0 12px 12px;">
                <For
                    each=move || { filtered.get().into_iter().enumerate().collect::<Vec<_>>() }
                    key=|item| item.1.0.clone()
                    children=move |item| {
                        let list_idx = item.0;
                        let name = item.1.0.clone();
                        let guild = item.1.1.clone();
                        let (r, g, b) = item.1.3;
                        let name_click = name.clone();
                        let on_click = move |_| {
                            detail_return_guild.set(None);
                            selected.set(Some(name_click.clone()));
                            let map = territories.get_untracked();
                            if let Some(ct) = map.get(&name_click) {
                                let loc = &ct.territory.location;
                                let (cw, ch) = canvas_dimensions();
                                viewport.update(|vp| {
                                    vp.fit_bounds(
                                        loc.left() as f64 - 200.0,
                                        loc.top() as f64 - 200.0,
                                        loc.right() as f64 + 200.0,
                                        loc.bottom() as f64 + 200.0,
                                        cw,
                                        ch,
                                    );
                                });
                            }
                        };
                        view! {
                            <div
                                data-sidebar-idx={list_idx.to_string()}
                                style="display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-radius: 4px; cursor: pointer; transition: background 0.15s, box-shadow 0.15s;"
                                style:box-shadow=move || if sidebar_index.get() == list_idx { "inset 2px 0 0 #f5c542" } else { "none" }
                                on:click=on_click
                                on:mouseenter=|e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("background", "#232738").ok();
                                    }
                                }
                                on:mouseleave=|e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("background", "transparent").ok();
                                    }
                                }
                            >
                                <div style={format!("width: 14px; height: 14px; border-radius: 3px; border: 1px solid rgba(255,255,255,0.1); flex-shrink: 0; box-shadow: inset 1px 1px 0 rgba(255,255,255,0.06), inset -1px -1px 0 rgba(0,0,0,0.3); background: {};", rgba_css(r, g, b, 0.8))} />
                                <div style="flex: 1; min-width: 0;">
                                    <div style="font-size: 1.021rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">{name}</div>
                                    <div style="font-size: 0.87rem; color: #9a9590; font-family: 'JetBrains Mono', monospace;">{guild}</div>
                                </div>
                            </div>
                        }
                    }
                />
            </div>
        </div>
    }
}

#[component]
fn LeaderboardPanel() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let SelectedGuild(selected_guild) = expect_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();
    let CurrentMode(mode) = expect_context();
    let ManualSrScalar(manual_sr_scalar) = expect_context();
    let AutoSrScalarEnabled(auto_sr_scalar_enabled) = expect_context();
    let LiveSeasonScalarSample(live_scalar_sample) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let ShowLeaderboardSrGain(show_leaderboard_sr_gain) = expect_context();
    let ShowLeaderboardSrValue(show_leaderboard_sr_value) = expect_context();
    let ShowLeaderboardTerritoryCount(show_leaderboard_territory_count) = expect_context();
    let ShowLeaderboardOnline(show_leaderboard_online) = expect_context();
    let GuildOnlineData(guild_online_data) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let LeaderboardSortBySr(sort_by_sr) = expect_context();

    let scalar_state = Memo::new(move |_| {
        effective_scalar(
            mode.get(),
            auto_sr_scalar_enabled.get(),
            manual_sr_scalar.get(),
            live_scalar_sample.get(),
            history_scalar_sample.get(),
        )
    });

    let leaderboard = Memo::new(move |_| {
        let map = territories.get();
        let online_data = guild_online_data.get();
        let history_sr = history_sr_leaderboard.get().unwrap_or_default();
        let sort_sr = sort_by_sr.get();
        let history_mode = mode.get() == MapMode::History;
        let mut guild_counts: HashMap<String, _> = HashMap::new();
        let history_sr_map: HashMap<String, (i64, u32, Option<i64>)> = history_sr
            .into_iter()
            .map(|entry| {
                (
                    entry.guild_name,
                    (entry.season_rating, entry.season_rank, entry.sr_gain_5m),
                )
            })
            .collect();

        for ct in map.values() {
            let entry = guild_counts
                .entry(ct.territory.guild.name.clone())
                .or_insert_with(|| {
                    (
                        ct.territory.guild.name.clone(),
                        ct.territory.guild.prefix.clone(),
                        0,
                        ct.guild_color,
                    )
                });
            entry.2 += 1;
        }

        let mut sorted: Vec<_> = guild_counts.into_values().collect();

        if sort_sr {
            if history_mode {
                sorted.sort_by(|a, b| {
                    let sr_a = history_sr_map.get(&a.0).map(|value| value.0);
                    let sr_b = history_sr_map.get(&b.0).map(|value| value.0);
                    match (sr_a, sr_b) {
                        (Some(sr_a), Some(sr_b)) => sr_b
                            .cmp(&sr_a)
                            .then_with(|| b.2.cmp(&a.2))
                            .then_with(|| a.0.cmp(&b.0)),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)),
                    }
                });
            } else {
                sorted.sort_by(|a, b| {
                    let sr_a = online_data
                        .get(&a.0)
                        .and_then(|d| d.season_rating)
                        .unwrap_or(0);
                    let sr_b = online_data
                        .get(&b.0)
                        .and_then(|d| d.season_rating)
                        .unwrap_or(0);
                    sr_b.cmp(&sr_a)
                        .then_with(|| b.2.cmp(&a.2))
                        .then_with(|| a.0.cmp(&b.0))
                });
            }
        } else {
            sorted.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        }
        sorted.truncate(20);

        let scalar = scalar_state.get().value;
        sorted
            .into_iter()
            .map(|(name, prefix, count, color)| {
                let passive_sr_h = passive_sr_per_hour(count as usize, scalar);
                let (sr, season_rank, sr_gain_5m) = if history_mode {
                    history_sr_map
                        .get(&name)
                        .copied()
                        .map_or((None, None, None), |(rating, rank, gain)| {
                            (Some(rating), Some(rank), gain)
                        })
                } else {
                    (
                        online_data.get(&name).and_then(|d| d.season_rating),
                        None,
                        None,
                    )
                };
                (
                    name,
                    prefix,
                    count,
                    color,
                    passive_sr_h,
                    sr,
                    season_rank,
                    sr_gain_5m,
                )
            })
            .collect::<Vec<_>>()
    });

    // Sync sidebar items for keyboard navigation (guild names for leaderboard)
    Effect::new(move || {
        let lb = leaderboard.get();
        let items: Vec<String> = lb
            .iter()
            .map(|(name, _, _, _, _, _, _, _)| name.clone())
            .collect();
        let prev = sidebar_items.get_untracked();
        if items != prev {
            sidebar_index.set(0);
        }
        sidebar_items.set(items);
    });

    let is_empty = Memo::new(move |_| territories.get().is_empty());

    view! {
        <div style="border-bottom: 1px solid #282c3e;">
            <div style="display: flex; align-items: center; justify-content: space-between; padding: 14px 24px 8px;">
                <div style="font-family: 'Silkscreen', monospace; font-size: 0.986rem; text-transform: uppercase; letter-spacing: 0.14em; color: #5a5860;">
                    <span style="color: #f5c542; margin-right: 6px; font-size: 0.812rem;">{"\u{25C6}"}</span>"Top Guilds"
                </div>
                <div style="display: flex; gap: 4px;">
                    <span
                        style=move || {
                            let active = !sort_by_sr.get();
                            format!(
                                "font-family: 'JetBrains Mono', monospace; font-size: 0.754rem; padding: 2px 8px; border-radius: 3px; cursor: pointer; transition: color 0.15s, background 0.15s; {}",
                                if active {
                                    "color: #f5c542; background: rgba(245,197,66,0.1);"
                                } else {
                                    "color: #3a3f5c; background: transparent;"
                                }
                            )
                        }
                        on:click=move |_| sort_by_sr.set(false)
                    >"Territories"</span>
                    <span
                        style=move || {
                            let active = sort_by_sr.get();
                            format!(
                                "font-family: 'JetBrains Mono', monospace; font-size: 0.754rem; padding: 2px 8px; border-radius: 3px; cursor: pointer; transition: color 0.15s, background 0.15s; {}",
                                if active {
                                    "color: var(--accent-live); background: rgba(var(--accent-live-rgb),0.1);"
                                } else {
                                    "color: #3a3f5c; background: transparent;"
                                }
                            )
                        }
                        on:click=move |_| sort_by_sr.set(true)
                    >"SR"</span>
                </div>
            </div>
            <Show
                when=move || !is_empty.get()
                fallback=|| view! {
                    <div style="padding: 24px; text-align: center;">
                        <div class="status-pulse" style="font-family: 'JetBrains Mono', monospace; font-size: 0.905rem; color: #3a3f5c; letter-spacing: 0.05em;">"Awaiting territory data..."</div>
                        <div style="margin-top: 12px; display: flex; justify-content: center; gap: 4px;">
                            <div style="width: 4px; height: 4px; border-radius: 50%; background: #f5c542; opacity: 0.3; animation: pulse-dot 1.5s ease-in-out infinite;" />
                            <div style="width: 4px; height: 4px; border-radius: 50%; background: #f5c542; opacity: 0.3; animation: pulse-dot 1.5s ease-in-out 0.3s infinite;" />
                            <div style="width: 4px; height: 4px; border-radius: 50%; background: #f5c542; opacity: 0.3; animation: pulse-dot 1.5s ease-in-out 0.6s infinite;" />
                        </div>
                    </div>
                }
            >
            <ul style="list-style: none; padding: 0 12px 12px;">
                <For
                    each=move || {
                        leaderboard.get().into_iter().enumerate().collect::<Vec<_>>()
                    }
                    key=|item| (item.0, item.1.0.clone())
                    children=move |item| {
                        let list_idx = item.0;
                        let (
                            name,
                            prefix,
                            count,
                            (r, g, b),
                            passive_sr_h,
                            season_rating,
                            _season_rank,
                            sr_gain_5m,
                        ) = item.1;
                        let computed_rank = list_idx + 1;
                        let display_rank = u32::try_from(computed_rank).unwrap_or(u32::MAX);
                        let name_for_click = name.clone();
                        let name_for_online = name.clone();
                        let sr_badge = if mode.get_untracked() == MapMode::History {
                            sr_gain_5m
                                .map(|gain| {
                                    if gain >= 0 {
                                        format!("+{}/5m", format_sr_value(gain))
                                    } else {
                                        format!("-{}/5m", format_sr_value(-gain))
                                    }
                                })
                                .unwrap_or_else(|| "--".to_string())
                        } else {
                            format!("{}/h", format_sr_rate(passive_sr_h))
                        };
                        let sr_display = season_rating.map(format_sr_value);
                        let on_click = move |_| {
                            selected_guild.set(Some(name_for_click.clone()));
                        };
                        let rank_class = match display_rank {
                            1 => "text-gold-gradient",
                            2 => "text-silver-gradient",
                            3 => "text-bronze-gradient",
                            _ => "",
                        };
                        let rank_style = if display_rank > 3 {
                            "font-family: 'JetBrains Mono', monospace; font-size: 0.87rem; color: #4a4e6a; width: 26px; text-align: right; flex-shrink: 0;"
                        } else {
                            "font-family: 'JetBrains Mono', monospace; font-size: 0.87rem; font-weight: 700; width: 26px; text-align: right; flex-shrink: 0;"
                        };
                        let row_style = if mode.get_untracked() == MapMode::History {
                            "display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-radius: 4px; cursor: pointer; transition: background 0.15s, box-shadow 0.15s;".to_string()
                        } else {
                            let delay_ms = computed_rank * 30;
                            format!(
                                "display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-radius: 4px; cursor: pointer; transition: background 0.15s, box-shadow 0.15s; animation: fade-in-up 0.3s ease-out {}ms both;",
                                delay_ms
                            )
                        };
                        // Top 3 get a subtle left accent
                        let is_podium = display_rank <= 3;
                        let color_bar_style = if is_podium {
                            format!(
                                "width: 3px; height: 100%; border-radius: 2px; background: {}; flex-shrink: 0; align-self: stretch;",
                                rgba_css(r, g, b, 0.6)
                            )
                        } else {
                            String::new()
                        };
                        view! {
                            <li
                                data-sidebar-idx={list_idx.to_string()}
                                style=row_style
                                style:box-shadow=move || if sidebar_index.get() == list_idx { "inset 2px 0 0 #f5c542" } else { "none" }
                                on:click=on_click
                                on:mouseenter=|e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("background", "#232738").ok();
                                    }
                                }
                                on:mouseleave=|e| {
                                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                        el.style().set_property("background", "transparent").ok();
                                    }
                                }
                            >
                                {is_podium.then(|| view! { <div style=color_bar_style.clone() /> })}
                                <span class=rank_class style=rank_style>{display_rank}</span>
                                <div style={format!("width: 16px; height: 16px; border-radius: 3px; border: 1px solid rgba(255,255,255,0.1); flex-shrink: 0; box-shadow: 0 0 4px {}, inset 1px 1px 0 rgba(255,255,255,0.06), inset -1px -1px 0 rgba(0,0,0,0.3); background: {};", rgba_css(r, g, b, 0.15), rgba_css(r, g, b, 0.8))} />
                                <span style="flex: 1; font-size: 1.044rem; color: #e2e0d8; font-family: 'Inter', system-ui, sans-serif; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">{name}</span>
                                <span style="font-size: 0.812rem; color: #9a9590; font-family: 'JetBrains Mono', monospace;">"[" {prefix} "]"</span>
                                {move || show_leaderboard_sr_gain.get().then(|| view! {
                                    <span style="font-size: 0.766rem; color: var(--accent-live); font-family: 'JetBrains Mono', monospace; background: rgba(var(--accent-live-rgb),0.09); border: 1px solid rgba(var(--accent-live-rgb),0.2); padding: 1px 5px; border-radius: 3px; min-width: 58px; text-align: center;">
                                        {sr_badge.clone()}
                                    </span>
                                })}
                                {move || {
                                    let show_sr = sort_by_sr.get() || show_leaderboard_sr_value.get();
                                    show_sr.then(|| sr_display.clone().map(|sr| view! {
                                        <span style="font-size: 0.766rem; color: var(--accent-live); font-family: 'JetBrains Mono', monospace; background: rgba(var(--accent-live-rgb),0.06); padding: 1px 5px; border-radius: 3px; min-width: 40px; text-align: center;">
                                            {sr}
                                        </span>
                                    }))
                                }}
                                {move || show_leaderboard_territory_count.get().then(|| view! {
                                    <span style="font-size: 0.951rem; color: #f5c542; font-family: 'JetBrains Mono', monospace; min-width: 24px; text-align: right; font-weight: 500; background: rgba(245,197,66,0.06); padding: 1px 6px; border-radius: 3px;">{count}</span>
                                })}
                                {move || {
                                    if !show_leaderboard_online.get() {
                                        return None;
                                    }
                                    let data = guild_online_data.get();
                                    data.get(&name_for_online).map(|info| view! {
                                        <span style="font-size: 0.766rem; color: #50c878; font-family: 'JetBrains Mono', monospace; background: rgba(80,200,120,0.06); padding: 1px 5px; border-radius: 3px; min-width: 28px; text-align: center; display: inline-flex; align-items: center; gap: 3px;">
                                            <span style="font-size: 0.522rem; line-height: 1;">{"\u{25CF}"}</span>
                                            {info.online}
                                        </span>
                                    })
                                }}
                            </li>
                        }
                    }
                />
            </ul>
            </Show>
        </div>
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OnlineMemberRow {
    username: String,
    rank_label: String,
    rank_priority: u8,
    server: String,
}

fn rank_label_and_priority(rank_key: &str) -> Option<(&'static str, u8)> {
    match rank_key {
        "owner" => Some(("Owner", 0)),
        "chief" => Some(("Chief", 1)),
        "strategist" => Some(("Strategist", 2)),
        "captain" => Some(("Captain", 3)),
        "recruiter" => Some(("Recruiter", 4)),
        "recruit" => Some(("Recruit", 5)),
        _ => None,
    }
}

fn extract_online_members(json: &serde_json::Value) -> Vec<OnlineMemberRow> {
    let Some(members) = json.get("members").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for rank in &[
        "owner",
        "chief",
        "strategist",
        "captain",
        "recruiter",
        "recruit",
    ] {
        let Some((rank_label, rank_priority)) = rank_label_and_priority(rank) else {
            continue;
        };
        let Some(rank_obj) = members.get(*rank).and_then(|v| v.as_object()) else {
            continue;
        };
        for (username, info) in rank_obj {
            if info.get("online").and_then(|v| v.as_bool()) == Some(true) {
                let server = info
                    .get("server")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                result.push(OnlineMemberRow {
                    username: username.clone(),
                    rank_label: rank_label.to_string(),
                    rank_priority,
                    server,
                });
            }
        }
    }
    result.sort_by(|a, b| {
        a.rank_priority
            .cmp(&b.rank_priority)
            .then_with(|| {
                a.username
                    .to_ascii_lowercase()
                    .cmp(&b.username.to_ascii_lowercase())
            })
            .then_with(|| a.username.cmp(&b.username))
    });
    result
}

#[component]
fn GuildPanel() -> impl IntoView {
    let SelectedGuild(selected_guild) = expect_context();
    let Selected(selected) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let viewport: RwSignal<Viewport> = expect_context();
    let tick: RwSignal<i64> = expect_context();

    let guild_detail: RwSignal<Option<serde_json::Value>> = RwSignal::new(None);
    let guild_loading: RwSignal<bool> = RwSignal::new(false);
    let guild_request_nonce: RwSignal<u64> = RwSignal::new(0);

    // Fetch guild detail when selected_guild changes
    Effect::new(move || {
        let request_nonce = guild_request_nonce.get_untracked().wrapping_add(1);
        guild_request_nonce.set(request_nonce);

        let Some(name) = selected_guild.get() else {
            guild_detail.set(None);
            guild_loading.set(false);
            return;
        };
        guild_loading.set(true);
        guild_detail.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            let url = format!(
                "/api/guild/{}",
                js_sys::encode_uri_component(&name)
                    .as_string()
                    .unwrap_or_default()
            );
            let detail = match gloo_net::http::Request::get(&url).send().await {
                Ok(resp) if resp.ok() => resp.json::<serde_json::Value>().await.ok(),
                _ => None,
            };

            if guild_request_nonce.get_untracked() != request_nonce
                || selected_guild.get_untracked().as_deref() != Some(name.as_str())
            {
                return;
            }

            guild_detail.set(detail);
            guild_loading.set(false);
        });
    });

    let on_close = move |_| {
        selected_guild.set(None);
        sidebar_transient.set(false);
    };

    // Guild color from territory data
    let guild_color = Memo::new(move |_| {
        let name = selected_guild.get()?;
        let map = territories.get();
        let ct = map.values().find(|ct| ct.territory.guild.name == name)?;
        Some(ct.guild_color)
    });

    // Guild territories
    let guild_territories = Memo::new(move |_| {
        let name = selected_guild.get().unwrap_or_default();
        let map = territories.get();
        let mut terrs: Vec<(String, (u8, u8, u8))> = map
            .iter()
            .filter(|(_, ct)| ct.territory.guild.name == name)
            .map(|(tn, ct)| (tn.clone(), ct.guild_color))
            .collect();
        terrs.sort_by(|a, b| a.0.cmp(&b.0));
        terrs
    });

    view! {
        <div class="panel-reveal" style="border-bottom: 1px solid #282c3e; position: relative;">
            // Close button
            <button
                style="position: absolute; top: 12px; right: 12px; background: none; border: none; color: #5a5860; cursor: pointer; padding: 4px 8px; border-radius: 4px; transition: color 0.15s, background 0.15s; z-index: 1; display: flex; align-items: center; justify-content: center;"
                on:click=on_close
                on:mouseenter=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#e05252").ok();
                        el.style().set_property("background", "#232738").ok();
                    }
                }
                on:mouseleave=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#5a5860").ok();
                        el.style().set_property("background", "transparent").ok();
                    }
                }
            >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="16" height="16">
                    <path d="M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z" />
                </svg>
            </button>

            // Color accent bar
            {move || {
                guild_color.get().map(|(r, g, b)| {
                    let gradient = format!(
                        "height: 4px; background: linear-gradient(90deg, {}, transparent); border-radius: 2px 2px 0 0;",
                        rgba_css(r, g, b, 0.7)
                    );
                    view! { <div style=gradient /> }
                })
            }}

            <div style="padding: 16px 24px 12px;">
                // Guild name + color swatch
                <div style="display: flex; align-items: center; gap: 10px; margin-bottom: 6px;">
                    {move || {
                        guild_color.get().map(|(r, g, b)| {
                            let swatch = format!(
                                "width: 20px; height: 20px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.1); flex-shrink: 0; box-shadow: 0 0 6px {}, inset 1px 1px 0 rgba(255,255,255,0.06), inset -1px -1px 0 rgba(0,0,0,0.3); background: {};",
                                rgba_css(r, g, b, 0.2),
                                rgba_css(r, g, b, 0.8)
                            );
                            view! { <div style=swatch /> }
                        })
                    }}
                    {move || {
                        let guild_name = selected_guild.get().unwrap_or_default();
                        let guild_stats_url = crate::guild_stats_url(&guild_name);
                        view! {
                            <a href=guild_stats_url
                               target="_blank"
                               rel="noopener noreferrer"
                               title="Open Wynncraft guild stats"
                               style="font-family: 'Silkscreen', monospace; font-size: 1.188rem; color: #e2e0d8; text-decoration: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; transition: color 0.15s;"
                               on:mouseenter=|e| {
                                   if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                       el.style().set_property("color", "#50c878").ok();
                                   }
                               }
                               on:mouseleave=|e| {
                                   if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                       el.style().set_property("color", "#e2e0d8").ok();
                                   }
                               }
                            >
                                {guild_name}
                            </a>
                        }
                    }}
                </div>

                // Prefix · Level
                {move || {
                    guild_detail.get().map(|json| {
                        let prefix = json.get("prefix").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let level = json.get("level").and_then(|v| v.as_u64()).unwrap_or(0);
                        view! {
                            <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.905rem; color: #9a9590; margin-bottom: 10px;">
                                "[" {prefix} "]" " \u{00b7} Lv. " {level}
                            </div>
                        }
                    })
                }}

                // Online badge (prominent)
                {move || {
                    guild_detail.get().map(|json| {
                        let online = json.get("online").and_then(|v| v.as_u64()).unwrap_or(0);
                        view! {
                            <div style="display: flex; align-items: center; gap: 6px; margin-bottom: 14px;">
                                <span style="font-size: 0.638rem; color: #50c878; line-height: 1;">{"\u{25CF}"}</span>
                                <span style="font-family: 'JetBrains Mono', monospace; font-size: 1.102rem; color: #50c878; font-weight: 600;">
                                    {online}
                                </span>
                                <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.87rem; color: #50c878; opacity: 0.7;">
                                    "online"
                                </span>
                            </div>
                        }
                    })
                }}

                // Loading indicator
                {move || {
                    guild_loading.get().then(|| view! {
                        <div style="padding: 8px 0; text-align: center;">
                            <span class="status-pulse" style="font-family: 'JetBrains Mono', monospace; font-size: 0.905rem; color: #3a3f5c; letter-spacing: 0.05em;">"Loading guild data..."</span>
                        </div>
                    })
                }}

                // Stats rows
                {move || {
                    guild_detail.get().map(|json| {
                        let territories_count = guild_territories.get().len();
                        let members = json.get("members").and_then(|m| m.get("total")).and_then(|v| v.as_u64()).unwrap_or(0);
                        let wars = json.get("wars").and_then(|v| v.as_u64()).unwrap_or(0);
                        let season_rating = json
                            .get("seasonRanks")
                            .and_then(|v| v.as_object())
                            .and_then(|ranks| {
                                ranks
                                    .iter()
                                    .filter_map(|(k, v)| k.parse::<i32>().ok().map(|id| (id, v)))
                                    .max_by_key(|(id, _)| *id)
                            })
                            .and_then(|(season_id, v)| {
                                v.get("rating")
                                    .and_then(|r| r.as_i64())
                                    .map(|r| (season_id, r))
                            });
                        let created = json.get("created").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let reference_secs = tick.get();
                        let created_ago = if created.is_empty() {
                            String::new()
                        } else {
                            format_relative_time(&created, reference_secs)
                        };

                        view! {
                            <div style="display: flex; flex-direction: column; gap: 5px; margin-bottom: 14px; border-top: 1px solid #282c3e; padding-top: 12px;">
                                <div style="display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                    <span style="color: #5a5860;">"Territories"</span>
                                    <span style="color: #f5c542;">{territories_count}</span>
                                </div>
                                <div style="display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                    <span style="color: #5a5860;">"Members"</span>
                                    <span style="color: #e2e0d8;">{members}</span>
                                </div>
                                <div style="display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                    <span style="color: #5a5860;">"Wars"</span>
                                    <span style="color: #e2e0d8;">{wars}</span>
                                </div>
                                {season_rating.map(|(season_id, rating)| {
                                    let sr_text = format!("S{}: {}", season_id, format_sr_value(rating));
                                    view! {
                                        <div style="display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                            <span style="color: #5a5860;">"Season Rating"</span>
                                            <span style="color: var(--accent-live);">{sr_text}</span>
                                        </div>
                                    }
                                })}
                                <div style="display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                    <span style="color: #5a5860;">"Created"</span>
                                    <span style="color: #9a9590;">{created_ago}</span>
                                </div>
                            </div>
                        }
                    })
                }}

                // Online Members section
                {move || {
                    guild_detail.get().map(|json| {
                        let online_members = extract_online_members(&json);
                        if online_members.is_empty() {
                            return view! { <div /> }.into_any();
                        }
                        view! {
                            <div style="margin-bottom: 14px;">
                                <div style="font-family: 'Silkscreen', monospace; font-size: 0.87rem; text-transform: uppercase; letter-spacing: 0.14em; color: #5a5860; margin-bottom: 8px;">
                                    <span style="color: #50c878; margin-right: 6px; font-size: 0.696rem;">{"\u{25C6}"}</span>"Online Members"
                                </div>
                                <div style="display: flex; flex-direction: column; gap: 3px;">
                                    {online_members.into_iter().map(|member| {
                                        let username = member.username;
                                        let rank_label = member.rank_label;
                                        let server = member.server;
                                        let profile_url = format!("https://wynncraft.com/stats/player/{}", username);
                                        view! {
                                            <div style="display: flex; align-items: center; gap: 8px; padding: 3px 0; font-family: 'JetBrains Mono', monospace; font-size: 0.87rem;">
                                                <span style="font-size: 0.464rem; color: #50c878; line-height: 1;">{"\u{25CF}"}</span>
                                                <span style="color: #9a9590; font-size: 0.812rem; min-width: 72px;">"[" {rank_label} "]"</span>
                                                <a href=profile_url
                                                   target="_blank"
                                                   rel="noopener noreferrer"
                                                   style="flex: 1; color: #e2e0d8; text-decoration: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; transition: color 0.15s;"
                                                   on:mouseenter=|e| {
                                                       if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                                           el.style().set_property("color", "#50c878").ok();
                                                       }
                                                   }
                                                   on:mouseleave=|e| {
                                                       if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                                           el.style().set_property("color", "#e2e0d8").ok();
                                                       }
                                                   }
                                                >
                                                    {username}
                                                </a>
                                                <span style="color: #5a5860; font-size: 0.812rem;">{server}</span>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            </div>
                        }.into_any()
                    })
                }}

                // Territories section
                {move || {
                    let terrs = guild_territories.get();
                    if terrs.is_empty() {
                        return view! { <div /> }.into_any();
                    }
                    view! {
                        <div>
                            <div style="font-family: 'Silkscreen', monospace; font-size: 0.87rem; text-transform: uppercase; letter-spacing: 0.14em; color: #5a5860; margin-bottom: 8px; border-top: 1px solid #282c3e; padding-top: 12px;">
                                <span style="color: #f5c542; margin-right: 6px; font-size: 0.696rem;">{"\u{25C6}"}</span>"Territories"
                            </div>
                            <div style="display: flex; flex-direction: column; gap: 2px;">
                                {terrs.into_iter().map(|(territory_name, (r, g, b))| {
                                    let tn = territory_name.clone();
                                    let on_terr_click = move |_| {
                                        let tn_inner = tn.clone();
                                        detail_return_guild.set(selected_guild.get_untracked());
                                        selected.set(Some(tn_inner.clone()));
                                        if !sidebar_open.get_untracked() {
                                            sidebar_open.set(true);
                                        }
                                        let map = territories.get_untracked();
                                        if let Some(ct) = map.get(&tn_inner) {
                                            let loc = &ct.territory.location;
                                            let (cw, ch) = canvas_dimensions();
                                            viewport.update(|vp| {
                                                vp.fit_bounds(
                                                    loc.left() as f64 - 200.0,
                                                    loc.top() as f64 - 200.0,
                                                    loc.right() as f64 + 200.0,
                                                    loc.bottom() as f64 + 200.0,
                                                    cw,
                                                    ch,
                                                );
                                            });
                                        }
                                    };
                                    let swatch = format!(
                                        "width: 10px; height: 10px; border-radius: 2px; flex-shrink: 0; background: {};",
                                        rgba_css(r, g, b, 0.7)
                                    );
                                    view! {
                                        <div
                                            style="display: flex; align-items: center; gap: 8px; padding: 4px 6px; border-radius: 3px; cursor: pointer; transition: background 0.15s; font-family: 'JetBrains Mono', monospace; font-size: 0.87rem; color: #e2e0d8;"
                                            on:click=on_terr_click
                                            on:mouseenter=|e| {
                                                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                                    el.style().set_property("background", "#232738").ok();
                                                }
                                            }
                                            on:mouseleave=|e| {
                                                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                                                    el.style().set_property("background", "transparent").ok();
                                                }
                                            }
                                        >
                                            <div style=swatch />
                                            <span style="overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">{territory_name}</span>
                                        </div>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        </div>
                    }.into_any()
                }}
            </div>
        </div>
    }
}

#[component]
fn DetailPanel() -> impl IntoView {
    let Selected(selected) = expect_context();
    let SelectedGuild(selected_guild) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let tick: RwSignal<i64> = expect_context();
    let CurrentMode(mode) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let ManualSrScalar(manual_sr_scalar) = expect_context();
    let AutoSrScalarEnabled(auto_sr_scalar_enabled) = expect_context();
    let LiveSeasonScalarSample(live_scalar_sample) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();

    let tower_state: crate::tower::TowerState = expect_context();

    let scalar_state = Memo::new(move |_| {
        effective_scalar(
            mode.get(),
            auto_sr_scalar_enabled.get(),
            manual_sr_scalar.get(),
            live_scalar_sample.get(),
            history_scalar_sample.get(),
        )
    });

    // Guild-aware connection counts — only recomputes when territories/selection change, not every tick
    let guild_counts = Memo::new(move |_| {
        let name = selected.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let guild_uuid = ct.territory.guild.uuid.as_str();
        let connections = ct.territory.connections.as_slice();

        let (guild_conn, total_conn, ext) =
            sequoia_shared::tower::count_guild_connections(&name, connections, guild_uuid, |n| {
                let ct2 = map.get(n)?;
                Some((
                    ct2.territory.guild.uuid.as_str(),
                    ct2.territory.connections.as_slice(),
                ))
            });

        Some((guild_conn, total_conn, ext))
    });

    // Sync tower connection counts when territory changes
    Effect::new(move || {
        if let Some((gc, _, ext)) = guild_counts.get() {
            tower_state.connections.set(gc);
            tower_state.externals.set(ext);
        }
    });

    let detail = move || {
        let reference_secs = if mode.get() == MapMode::History {
            history_timestamp.get().unwrap_or_else(|| tick.get())
        } else {
            tick.get()
        };
        let name = selected.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let takes_in_window = if heat_mode_enabled.get() {
            Some(
                heat_entries_by_territory
                    .get()
                    .get(&name)
                    .copied()
                    .unwrap_or(0),
            )
        } else {
            None
        };
        let acquired_rfc = ct.territory.acquired.to_rfc3339();
        let guild_uuid = ct.territory.guild.uuid.clone();
        let guild_name = ct.territory.guild.name.clone();
        let guild_territory_count = map
            .values()
            .filter(|candidate| {
                if guild_uuid.is_empty() {
                    candidate.territory.guild.name == guild_name
                } else {
                    candidate.territory.guild.uuid == guild_uuid
                }
            })
            .count();
        let treasury = chrono::DateTime::parse_from_rfc3339(&acquired_rfc)
            .ok()
            .map(|dt| {
                let secs = (reference_secs - dt.timestamp()).max(0);
                TreasuryLevel::from_held_seconds(secs)
            })
            .unwrap_or(TreasuryLevel::VeryLow);
        Some((
            name,
            ct.territory.guild.name.clone(),
            ct.territory.guild.prefix.clone(),
            ct.territory.guild.uuid.clone(),
            acquired_rfc,
            ct.territory.location.clone(),
            ct.guild_color,
            treasury,
            ct.territory.resources.clone(),
            ct.territory.runtime.clone(),
            ct.territory.connections.len(),
            guild_territory_count,
            reference_secs,
            takes_in_window,
        ))
    };

    let current_territory_guild = Memo::new(move |_| {
        let name = selected.get()?;
        let map = territories.get();
        map.get(&name).map(|ct| ct.territory.guild.name.clone())
    });

    let on_back = move |_| {
        let owner_guild = current_territory_guild.get_untracked();
        selected.set(None);
        if let Some(guild_name) = owner_guild {
            selected_guild.set(Some(guild_name));
        }
        detail_return_guild.set(None);
        sidebar_transient.set(false);
    };

    let on_close = move |_| {
        if let Some(return_guild) = detail_return_guild.get_untracked() {
            selected.set(None);
            selected_guild.set(Some(return_guild));
        } else {
            selected.set(None);
        }
        detail_return_guild.set(None);
        sidebar_transient.set(false);
    };

    view! {
        <div class="panel-reveal" style="border-bottom: 1px solid #282c3e; position: relative;">
            <button
                title="Back to guild"
                aria-label="Back to guild"
                style="position: absolute; top: 12px; left: 12px; background: #1a1d2a; border: 1px solid #282c3e; color: #9a9590; cursor: pointer; padding: 3px 8px; border-radius: 4px; transition: color 0.15s, background 0.15s, border-color 0.15s; z-index: 1; display: flex; align-items: center; gap: 5px; font-family: 'JetBrains Mono', monospace; font-size: 0.777rem; text-transform: uppercase; letter-spacing: 0.08em;"
                on:click=on_back
                on:mouseenter=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#f5c542").ok();
                        el.style().set_property("background", "#232738").ok();
                        el.style().set_property("border-color", "#3a3f5c").ok();
                    }
                }
                on:mouseleave=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#9a9590").ok();
                        el.style().set_property("background", "#1a1d2a").ok();
                        el.style().set_property("border-color", "#282c3e").ok();
                    }
                }
            >
                <span style="font-size: 0.928rem; line-height: 1;">{"\u{2039}"}</span>
                <span>"Back"</span>
            </button>
            <button
                style="position: absolute; top: 12px; right: 12px; background: none; border: none; color: #5a5860; cursor: pointer; padding: 4px 8px; border-radius: 4px; transition: color 0.15s, background 0.15s; z-index: 1; display: flex; align-items: center; justify-content: center;"
                on:click=on_close
                on:mouseenter=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#e05252").ok();
                        el.style().set_property("background", "#232738").ok();
                    }
                }
                on:mouseleave=|e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#5a5860").ok();
                        el.style().set_property("background", "transparent").ok();
                    }
                }
            >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="16" height="16">
                    <path d="M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z" />
                </svg>
            </button>
            {move || {
                detail()
                    .map(|(name, guild_name, guild_prefix, _uuid, acquired, location, (r, g, b), treasury, resources, runtime, conn_count, guild_territory_count, reference_secs, takes_in_window)| {
                        let relative_time = format_relative_time(&acquired, reference_secs);
                        let (tr, tg, tb) = treasury.color_rgb();
                        let treasury_label = treasury.label();
                        let buff = treasury.buff_percent();
                        let scalar_details = scalar_state.get();
                        let passive_sr_h = passive_sr_per_hour(guild_territory_count, scalar_details.value);
                        let passive_sr_5s = passive_sr_per_5s(guild_territory_count, scalar_details.value);
                        let passive_sr_label = format!(
                            "{}/h \u{00b7} {}/5s",
                            format_sr_rate(passive_sr_h),
                            format_sr_rate(passive_sr_5s)
                        );
                        let scalar_note = match (scalar_details.source, scalar_details.sample.as_ref()) {
                            (ScalarSource::Manual, _) => {
                                format!("Manual scalar {:.2} in use", scalar_details.value)
                            }
                            (ScalarSource::LiveEstimate, Some(sample)) => {
                                let sampled = format_relative_time(&sample.sampled_at, reference_secs);
                                format!(
                                    "Live estimate S{}/ weighted {:.2}, raw {:.2}, conf {:.0}% ({})",
                                    sample.season_id,
                                    sample.scalar_weighted,
                                    sample.scalar_raw,
                                    sample.confidence * 100.0,
                                    sampled
                                )
                            }
                            (ScalarSource::HistoryEstimate, Some(sample)) => {
                                let sampled = format_relative_time(&sample.sampled_at, reference_secs);
                                format!(
                                    "History estimate S{}/ weighted {:.2}, raw {:.2}, conf {:.0}% ({})",
                                    sample.season_id,
                                    sample.scalar_weighted,
                                    sample.scalar_raw,
                                    sample.confidence * 100.0,
                                    sampled
                                )
                            }
                            (_, None) => {
                                format!("Manual fallback scalar {:.2} (estimate unavailable)", scalar_details.value)
                            }
                        };

                        let runtime_hq = runtime.as_ref().and_then(|runtime| runtime.headquarters);
                        let runtime_defense = runtime
                            .as_ref()
                            .and_then(|runtime| runtime.defense_tier.clone());
                        let runtime_held_resources = runtime
                            .as_ref()
                            .and_then(|runtime| runtime.held_resources.clone())
                            .filter(|value| !value.is_empty());
                        let runtime_prod_resources = runtime
                            .as_ref()
                            .and_then(|runtime| runtime.production_rates.clone())
                            .filter(|value| !value.is_empty());
                        let runtime_capacity_resources = runtime
                            .as_ref()
                            .and_then(|runtime| runtime.storage_capacity.clone())
                            .filter(|value| !value.is_empty());
                        let runtime_provenance = runtime
                            .as_ref()
                            .and_then(|runtime| runtime.provenance.clone())
                            .map(|provenance| {
                                let source = if provenance.source.trim().is_empty() {
                                    "unknown".to_string()
                                } else {
                                    provenance.source.clone()
                                };
                                let observed_label = format_relative_time(
                                    &provenance.observed_at,
                                    reference_secs,
                                );
                                let visibility = visibility_label(&provenance);
                                let confidence = format_confidence(provenance.confidence);
                                let reporter_count = provenance.reporter_count;
                                (
                                    format!(
                                        "{} source \u{00b7} {} \u{00b7} conf {}",
                                        visibility, source, confidence
                                    ),
                                    format!(
                                        "Observed {} \u{00b7} reporters {}",
                                        observed_label, reporter_count
                                    ),
                                )
                            });

                        // Cooldown: territory can be queued again after 10 minutes
                        let cooldown = chrono::DateTime::parse_from_rfc3339(&acquired).ok().and_then(|dt| {
                            let age = (reference_secs - dt.timestamp()).max(0);
                            if age < 600 {
                                let remaining = 600 - age;
                                let frac = remaining as f64 / 600.0;
                                Some((format!("{}:{:02}", remaining / 60, remaining % 60), frac))
                            } else {
                                None
                            }
                        });
                        let guild_stats_url = crate::guild_stats_url(&guild_name);
                        view! {
                            // Guild color accent bar at top
                            <div style={format!(
                                "height: 4px; background: linear-gradient(to right, {}, {} 60%, transparent);",
                                rgba_css(r, g, b, 0.7),
                                rgba_css(r, g, b, 0.3),
                            )} />
                            <div style="padding: 40px 24px 20px;">
                                <div style="display: flex; align-items: center; gap: 10px; margin-bottom: 4px;">
                                    // Guild color swatch
                                    <div style={format!(
                                        "width: 20px; height: 20px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.12); background: {}; flex-shrink: 0; box-shadow: 0 0 6px {}, inset 1px 1px 0 rgba(255,255,255,0.06), inset -1px -1px 0 rgba(0,0,0,0.3);",
                                        rgba_css(r, g, b, 0.8),
                                        rgba_css(r, g, b, 0.3),
                                    )} />
                                    <div style="font-size: 1.242rem; font-weight: 700; color: #e2e0d8; font-family: 'Silkscreen', monospace;">{name}</div>
                                </div>
                                <a
                                    href=guild_stats_url
                                    target="_blank"
                                    rel="noopener noreferrer"
                                    title="Open Wynncraft guild stats"
                                    style="display: inline-block; font-size: 1.102rem; color: #f5c542; font-family: 'Inter', system-ui, sans-serif; margin-bottom: 2px; margin-left: 28px; text-decoration: none;"
                                >
                                    {guild_name}
                                </a>
                                <div style="font-size: 0.928rem; color: #9a9590; font-family: 'JetBrains Mono', monospace; margin-bottom: 16px; margin-left: 28px;">
                                    "[" {guild_prefix} "]"
                                </div>

                                // Detail rows with dotted leaders
                                <div style="display: flex; justify-content: space-between; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Acquired"</span>
                                    <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;" title=acquired>{relative_time}</span>
                                </div>
                                <div style="display: flex; justify-content: space-between; align-items: center; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Treasury"</span>
                                    <span style="display: flex; align-items: center; gap: 6px;">
                                        <span style={format!("color: {}; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;", rgba_css(tr, tg, tb, 1.0))}>{treasury_label}</span>
                                        {(buff > 0).then(|| view! {
                                            <span style={format!("font-size: 0.754rem; font-family: 'JetBrains Mono', monospace; color: {}; background: {}; padding: 1px 5px; border-radius: 3px;", rgba_css(tr, tg, tb, 0.9), rgba_css(tr, tg, tb, 0.08))}>{format!("+{}%", buff)}</span>
                                        })}
                                    </span>
                                </div>
                                <div style="display: flex; justify-content: space-between; align-items: center; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Passive SR"</span>
                                    <span style="color: var(--accent-live); font-family: 'JetBrains Mono', monospace; font-size: 0.882rem;">
                                        {passive_sr_label}
                                    </span>
                                </div>
                                {takes_in_window.map(|count| view! {
                                    <div style="display: flex; justify-content: space-between; align-items: center; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                        <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Takes in window"</span>
                                        <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem; font-variant-numeric: tabular-nums;">{count}</span>
                                    </div>
                                })}
                                <div style="padding: 6px 0 8px; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="font-size: 0.789rem; color: #7c829e; font-family: 'JetBrains Mono', monospace;">
                                        {scalar_note}
                                    </span>
                                </div>
                                {runtime_hq.map(|is_hq| view! {
                                    <div style="display: flex; justify-content: space-between; align-items: center; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                        <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Headquarters"</span>
                                        <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.835rem; color: #e2e0d8;">
                                            {if is_hq { "Yes" } else { "No" }}
                                        </span>
                                    </div>
                                })}
                                {runtime_defense.map(|defense_tier| view! {
                                    <div style="display: flex; justify-content: space-between; align-items: center; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                        <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Defense Tier"</span>
                                        <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.835rem; color: #e2e0d8;">{defense_tier}</span>
                                    </div>
                                })}
                                {runtime_provenance.map(|(primary, secondary)| view! {
                                    <div style="padding: 8px 0; border-bottom: 1px solid rgba(40,44,62,0.6); display: flex; flex-direction: column; gap: 2px;">
                                        <span style="font-size: 0.777rem; color: var(--accent-live); font-family: 'JetBrains Mono', monospace;">
                                            {primary}
                                        </span>
                                        <span style="font-size: 0.742rem; color: #7c829e; font-family: 'JetBrains Mono', monospace;">
                                            {secondary}
                                        </span>
                                    </div>
                                })}
                                {cooldown.map(|(remaining_text, frac)| view! {
                                    <div style="padding: 8px 0; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                        <div style="display: flex; justify-content: space-between; font-size: 0.986rem; margin-bottom: 6px;">
                                            <span style="color: #f5c542; font-family: 'Silkscreen', monospace;">"Cooldown"</span>
                                            <span style="color: #f5c542; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">{remaining_text}</span>
                                        </div>
                                        <div style="height: 5px; background: rgba(255,255,255,0.06); border-radius: 2px; overflow: hidden;">
                                            <div style={format!(
                                                "height: 100%; width: {:.1}%; background: linear-gradient(to right, #f5c542, #d4a030); border-radius: 2px; transition: width 1s linear; box-shadow: 0 0 6px rgba(245,197,66,0.1);",
                                                frac * 100.0
                                            )} />
                                        </div>
                                    </div>
                                })}
                                <div style="display: flex; justify-content: space-between; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Region"</span>
                                    <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                        {format!(
                                            "({}, {}) \u{2192} ({}, {})",
                                            location.start[0],
                                            location.start[1],
                                            location.end[0],
                                            location.end[1],
                                        )}
                                    </span>
                                </div>
                                <div style="display: flex; justify-content: space-between; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Size"</span>
                                    <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                        {format!(
                                            "{}\u{00d7}{}",
                                            location.width(),
                                            location.height(),
                                        )}
                                    </span>
                                </div>
                                <div style="display: flex; justify-content: space-between; padding: 8px 0; font-size: 0.986rem; border-bottom: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Connections"</span>
                                    <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.905rem;">
                                        {move || guild_counts.get().map(|(gc, tc, _)| format!("{}/{}", gc, tc)).unwrap_or_else(|| format!("{}", conn_count))}
                                    </span>
                                </div>
                                {(!resources.is_empty()).then(|| {
                                    let res_items = build_resource_items(&resources);
                                    view! {
                                        <div style="padding: 10px 0 4px;">
                                            <div style="font-family: 'Silkscreen', monospace; font-size: 0.986rem; text-transform: uppercase; letter-spacing: 0.12em; color: #5a5860; margin-bottom: 8px;">
                                                <span style="color: #f5c542; margin-right: 5px; font-size: 0.812rem;">{"\u{25C6}"}</span>"Resources"
                                            </div>
                                            <div style="display: flex; flex-wrap: wrap; gap: 6px;">
                                                {res_items.into_iter().map(|(label, value, icon_name)| {
                                                    let icon_style = icons::sprite_style(icon_name, 14).unwrap_or_default();
                                                    view! {
                                                        <div style="display: flex; align-items: center; gap: 5px; background: #1a1d2a; padding: 4px 8px; border-radius: 4px; border: 1px solid #282c3e;">
                                                            <span style={icon_style} />
                                                            <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.812rem; color: #e2e0d8;">{value}</span>
                                                            <span style="font-family: 'Inter', system-ui, sans-serif; font-size: 0.719rem; color: #5a5860;">{label}</span>
                                                        </div>
                                                    }
                                                }).collect::<Vec<_>>()}
                                            </div>
                                        </div>
                                    }
                                })}
                                {runtime_held_resources.clone().map(|held| {
                                    let held_items = build_resource_items(&held);
                                    view! {
                                        <div style="padding: 8px 0 4px;">
                                            <div style="font-family: 'Silkscreen', monospace; font-size: 0.835rem; text-transform: uppercase; letter-spacing: 0.1em; color: var(--accent-live); margin-bottom: 6px;">
                                                "Held Resources (Live)"
                                            </div>
                                            <div style="display: flex; flex-wrap: wrap; gap: 6px;">
                                                {held_items.into_iter().map(|(label, value, icon_name)| {
                                                    let icon_style = icons::sprite_style(icon_name, 14).unwrap_or_default();
                                                    view! {
                                                        <div style="display: flex; align-items: center; gap: 5px; background: #1a1d2a; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(var(--accent-live-rgb),0.25);">
                                                            <span style={icon_style} />
                                                            <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.812rem; color: #e2e0d8;">{value}</span>
                                                            <span style="font-family: 'Inter', system-ui, sans-serif; font-size: 0.719rem; color: var(--accent-live);">{label}</span>
                                                        </div>
                                                    }
                                                }).collect::<Vec<_>>()}
                                            </div>
                                        </div>
                                    }
                                })}
                                {runtime_prod_resources.clone().map(|prod| {
                                    let prod_items = build_resource_items(&prod);
                                    view! {
                                        <div style="padding: 8px 0 4px;">
                                            <div style="font-family: 'Silkscreen', monospace; font-size: 0.835rem; text-transform: uppercase; letter-spacing: 0.1em; color: var(--accent-live); margin-bottom: 6px;">
                                                "Production/Hr (Live)"
                                            </div>
                                            <div style="display: flex; flex-wrap: wrap; gap: 6px;">
                                                {prod_items.into_iter().map(|(label, value, icon_name)| {
                                                    let icon_style = icons::sprite_style(icon_name, 14).unwrap_or_default();
                                                    view! {
                                                        <div style="display: flex; align-items: center; gap: 5px; background: #1a1d2a; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(var(--accent-live-rgb),0.2);">
                                                            <span style={icon_style} />
                                                            <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.812rem; color: #e2e0d8;">{value}</span>
                                                            <span style="font-family: 'Inter', system-ui, sans-serif; font-size: 0.719rem; color: var(--accent-live);">{label}</span>
                                                        </div>
                                                    }
                                                }).collect::<Vec<_>>()}
                                            </div>
                                        </div>
                                    }
                                })}
                                {runtime_capacity_resources.clone().map(|cap| {
                                    let cap_items = build_resource_items(&cap);
                                    view! {
                                        <div style="padding: 8px 0 4px;">
                                            <div style="font-family: 'Silkscreen', monospace; font-size: 0.835rem; text-transform: uppercase; letter-spacing: 0.1em; color: var(--accent-live); margin-bottom: 6px;">
                                                "Storage Cap (Live)"
                                            </div>
                                            <div style="display: flex; flex-wrap: wrap; gap: 6px;">
                                                {cap_items.into_iter().map(|(label, value, icon_name)| {
                                                    let icon_style = icons::sprite_style(icon_name, 14).unwrap_or_default();
                                                    view! {
                                                        <div style="display: flex; align-items: center; gap: 5px; background: #1a1d2a; padding: 4px 8px; border-radius: 4px; border: 1px solid rgba(var(--accent-live-rgb),0.2);">
                                                            <span style={icon_style} />
                                                            <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.812rem; color: #e2e0d8;">{value}</span>
                                                            <span style="font-family: 'Inter', system-ui, sans-serif; font-size: 0.719rem; color: var(--accent-live);">{label}</span>
                                                        </div>
                                                    }
                                                }).collect::<Vec<_>>()}
                                            </div>
                                        </div>
                                    }
                                })}
                                <TowerCalculator />
                            </div>
                        }
                    })
            }}
        </div>
    }
}

#[component]
fn StatsBar() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let connection: RwSignal<ConnectionStatus> = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let CurrentMode(mode) = expect_context();
    let PlaybackActive(playback_active) = expect_context();
    let HistoryAvailable(history_available) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let HistoryBoundsSignal(history_bounds) = expect_context();
    let HistoryFetchNonce(history_fetch_nonce) = expect_context();
    let LastLiveSeq(last_live_seq) = expect_context();
    let HistoryBufferedUpdates(history_buffered_updates) = expect_context();
    let HistoryBufferModeActive(history_buffer_mode_active) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let NeedsLiveResync(needs_live_resync) = expect_context();
    let LiveHandoffResyncCount(live_handoff_resync_count) = expect_context();
    let TerritoryGeometryStore(geo_store) = expect_context();
    let GuildColorStore(guild_color_store) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let ResetSettingsTrigger(reset_settings_trigger) = expect_context();

    let guild_count = Memo::new(move |_| {
        let map = territories.get();
        let mut guilds: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for ct in map.values() {
            guilds.insert(&ct.territory.guild.name);
        }
        guilds.len()
    });

    let status_dot_style = Memo::new(move |_| match connection.get() {
        ConnectionStatus::Live => {
            "width: 8px; height: 8px; border-radius: 50%; background: #50c878; box-shadow: 0 0 8px rgba(80,200,120,0.5);"
        }
        ConnectionStatus::Connecting => {
            "width: 8px; height: 8px; border-radius: 50%; background: #f5c542; box-shadow: 0 0 8px rgba(245,197,66,0.35); animation: pulse-dot 1.5s ease-in-out infinite;"
        }
        ConnectionStatus::Reconnecting => {
            "width: 8px; height: 8px; border-radius: 50%; background: #f5c542; box-shadow: 0 0 8px rgba(245,197,66,0.35); animation: pulse-dot 1.5s ease-in-out infinite;"
        }
    });

    let status_text = Memo::new(move |_| match connection.get() {
        ConnectionStatus::Live => "Live",
        ConnectionStatus::Connecting => "Connecting...",
        ConnectionStatus::Reconnecting => "Reconnecting...",
    });

    let is_history = move || mode.get() == MapMode::History;

    view! {
        <div style="padding: 10px 12px; border-top: 1px solid #282c3e; display: flex; align-items: center; justify-content: space-between; gap: 8px; font-family: 'JetBrains Mono', monospace; font-size: 0.789rem; color: #6a6870;">
            <div style="display: flex; align-items: center; gap: 6px; min-width: 0; flex: 1; overflow-x: auto; scrollbar-width: none;">
            <button
                style:display=move || if history_available.get() && !is_mobile.get() { "flex" } else { "none" }
                style="background: none; border: 1px solid #282c3e; border-radius: 999px; padding: 5px 10px; cursor: pointer; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s; font-size: 0.766rem; min-width: 64px;"
                title=move || if is_history() { "Disable history mode (h)" } else { "Enable history mode (h)" }
                style:color=move || if is_history() { "#13161f" } else { "#5a5860" }
                style:background=move || if is_history() { "#f5c542" } else { "#1a1d2a" }
                style:border-color=move || if is_history() { "#f5c542" } else { "#282c3e" }
                style:box-shadow=move || if is_history() { "0 0 8px rgba(245,197,66,0.35)" } else { "none" }
                on:click=move |_| {
                    if is_history() {
                        history::exit_history_mode(history::ExitHistoryModeInput {
                            mode,
                            playback_active,
                            history_fetch_nonce,
                            history_timestamp,
                            history_buffered_updates,
                            history_buffer_mode_active,
                            last_live_seq,
                            needs_live_resync,
                            live_handoff_resync_count,
                            history_sr_leaderboard,
                            territories,
                        });
                    } else {
                        history::enter_history_mode(history::EnterHistoryModeInput {
                            mode,
                            history_timestamp,
                            history_bounds,
                            history_fetch_nonce,
                            history_buffered_updates,
                            history_buffer_mode_active,
                            needs_live_resync,
                            history_scalar_sample,
                            history_sr_leaderboard,
                            geo_store,
                            guild_color_store,
                            territories,
                        });
                    }
                }
                on:mouseenter=move |e| {
                    if !is_history()
                        && let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.style().set_property("color", "#9a9590").ok();
                        el.style().set_property("border-color", "#3a3f5c").ok();
                    }
                }
                on:mouseleave=move |e| {
                    if !is_history()
                        && let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.style().set_property("color", "#5a5860").ok();
                        el.style().set_property("border-color", "#282c3e").ok();
                    }
                }
            >
                "History"
            </button>
            <button
                style:display=move || if !is_mobile.get() { "flex" } else { "none" }
                style="background: none; border: 1px solid #282c3e; border-radius: 999px; padding: 5px 10px; cursor: pointer; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s; font-size: 0.766rem; min-width: 58px;"
                title=move || if heat_mode_enabled.get() { "Disable heat map" } else { "Enable heat map" }
                style:color=move || if heat_mode_enabled.get() { "#13161f" } else { "#5a5860" }
                style:background=move || if heat_mode_enabled.get() { "#f58c32" } else { "#1a1d2a" }
                style:border-color=move || if heat_mode_enabled.get() { "#f58c32" } else { "#282c3e" }
                style:box-shadow=move || if heat_mode_enabled.get() { "0 0 8px rgba(245,140,50,0.35)" } else { "none" }
                on:click=move |_| heat_mode_enabled.update(|v| *v = !*v)
                on:mouseenter=move |e| {
                    if !heat_mode_enabled.get()
                        && let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.style().set_property("color", "#9a9590").ok();
                        el.style().set_property("border-color", "#3a3f5c").ok();
                    }
                }
                on:mouseleave=move |e| {
                    if !heat_mode_enabled.get()
                        && let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.style().set_property("color", "#5a5860").ok();
                        el.style().set_property("border-color", "#282c3e").ok();
                    }
                }
            >
                "Heat"
            </button>
            <div
                style="background: #1a1d2a; border-radius: 999px; padding: 5px 10px; border: 1px solid #282c3e; display: flex; align-items: center; gap: 4px;"
                style:min-height=move || if is_mobile.get() { "44px" } else { "auto" }
            >
                <span style="color: #9a9590;">{move || guild_count.get()}</span>
                <span>" guilds"</span>
            </div>
            </div>
            <div style="display: flex; align-items: center; gap: 6px; flex-shrink: 0;">
            <div
                title=move || status_text.get()
                style="width: 26px; height: 26px; border: 1px solid #282c3e; border-radius: 999px; background: #1a1d2a; display: flex; align-items: center; justify-content: center; flex-shrink: 0;"
            >
                <span style=move || status_dot_style.get()></span>
            </div>
            <button
                style:display=move || if show_settings.get() { "flex" } else { "none" }
                style="background: linear-gradient(180deg, rgba(245,197,66,0.12) 0%, rgba(245,197,66,0.06) 100%); border: 1px solid rgba(245,197,66,0.24); border-radius: 999px; padding: 5px 11px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s, box-shadow 0.15s; font-family: 'Silkscreen', monospace; font-size: 0.648rem; letter-spacing: 0.08em; text-transform: uppercase; color: #f5c542; box-shadow: 0 0 12px rgba(245,197,66,0.08);"
                style:min-height=move || if is_mobile.get() { "44px" } else { "auto" }
                title="Reset all settings to defaults"
                on:click=move |_| {
                    reset_settings_trigger.update(|value| *value = value.saturating_add(1));
                }
                on:mouseenter=move |e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#13161f").ok();
                        el.style().set_property("background", "#f5c542").ok();
                        el.style().set_property("border-color", "#f5c542").ok();
                        el.style().set_property("box-shadow", "0 0 12px rgba(245,197,66,0.24)").ok();
                    }
                }
                on:mouseleave=move |e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                        el.style().set_property("color", "#f5c542").ok();
                        el.style().set_property("background", "linear-gradient(180deg, rgba(245,197,66,0.12) 0%, rgba(245,197,66,0.06) 100%)").ok();
                        el.style().set_property("border-color", "rgba(245,197,66,0.24)").ok();
                        el.style().set_property("box-shadow", "0 0 12px rgba(245,197,66,0.08)").ok();
                    }
                }
            >
                "Defaults"
            </button>
            <button
                style="background: none; border: 1px solid #282c3e; border-radius: 999px; padding: 5px 7px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s;"
                style:min-height=move || if is_mobile.get() { "44px" } else { "auto" }
                style:min-width=move || if is_mobile.get() { "44px" } else { "auto" }
                style:color=move || if show_settings.get() { "#f5c542" } else { "#5a5860" }
                style:background=move || if show_settings.get() { "rgba(245,197,66,0.06)" } else { "#1a1d2a" }
                style:border-color=move || if show_settings.get() { "rgba(245,197,66,0.25)" } else { "#282c3e" }
                on:click=move |_| show_settings.update(|v| *v = !*v)
                on:mouseenter=move |e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                        && !show_settings.get_untracked()
                    {
                        el.style().set_property("color", "#9a9590").ok();
                        el.style().set_property("border-color", "#3a3f5c").ok();
                    }
                }
                on:mouseleave=move |e| {
                    if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                        && !show_settings.get_untracked()
                    {
                        el.style().set_property("color", "#5a5860").ok();
                        el.style().set_property("border-color", "#282c3e").ok();
                    }
                }
            >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="14" height="14">
                    <path fill-rule="evenodd" d="M7.84 1.804A1 1 0 018.82 1h2.36a1 1 0 01.98.804l.331 1.652a6.993 6.993 0 011.929 1.115l1.598-.54a1 1 0 011.186.447l1.18 2.044a1 1 0 01-.205 1.251l-1.267 1.113a7.047 7.047 0 010 2.228l1.267 1.113a1 1 0 01.206 1.25l-1.18 2.045a1 1 0 01-1.187.447l-1.598-.54a6.993 6.993 0 01-1.929 1.115l-.33 1.652a1 1 0 01-.98.804H8.82a1 1 0 01-.98-.804l-.331-1.652a6.993 6.993 0 01-1.929-1.115l-1.598.54a1 1 0 01-1.186-.447l-1.18-2.044a1 1 0 01.205-1.251l1.267-1.114a7.05 7.05 0 010-2.227L1.821 7.773a1 1 0 01-.206-1.25l1.18-2.045a1 1 0 011.187-.447l1.598.54A6.993 6.993 0 017.51 3.456l.33-1.652zM10 13a3 3 0 100-6 3 3 0 000 6z" clip-rule="evenodd" />
                </svg>
            </button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::extract_online_members;
    use serde_json::json;

    #[test]
    fn extract_online_members_includes_only_online_members() {
        let payload = json!({
            "members": {
                "owner": {
                    "OwnerOne": { "online": true, "server": "NA1" },
                    "OwnerOffline": { "online": false, "server": "NA2" }
                },
                "chief": {
                    "ChiefOne": { "online": true, "server": "EU1" }
                },
                "recruit": {
                    "RecruitOne": { "online": true, "server": null }
                }
            }
        });

        let rows = extract_online_members(&payload);
        let usernames: Vec<&str> = rows.iter().map(|row| row.username.as_str()).collect();

        assert_eq!(usernames, vec!["OwnerOne", "ChiefOne", "RecruitOne"]);
        assert_eq!(rows[2].server, "");
    }

    #[test]
    fn extract_online_members_orders_by_rank_priority() {
        let payload = json!({
            "members": {
                "recruit": { "RecruitOne": { "online": true, "server": "AS1" } },
                "captain": { "CaptainOne": { "online": true, "server": "EU2" } },
                "owner": { "OwnerOne": { "online": true, "server": "NA1" } },
                "chief": { "ChiefOne": { "online": true, "server": "NA2" } },
                "recruiter": { "RecruiterOne": { "online": true, "server": "AS2" } },
                "strategist": { "StrategistOne": { "online": true, "server": "EU1" } }
            }
        });

        let rows = extract_online_members(&payload);
        let rank_labels: Vec<&str> = rows.iter().map(|row| row.rank_label.as_str()).collect();

        assert_eq!(
            rank_labels,
            vec![
                "Owner",
                "Chief",
                "Strategist",
                "Captain",
                "Recruiter",
                "Recruit"
            ]
        );
    }

    #[test]
    fn extract_online_members_orders_usernames_case_insensitively_within_rank() {
        let payload = json!({
            "members": {
                "chief": {
                    "beta": { "online": true, "server": "NA1" },
                    "Alpha": { "online": true, "server": "NA2" },
                    "alpha2": { "online": true, "server": "NA3" }
                }
            }
        });

        let rows = extract_online_members(&payload);
        let usernames: Vec<&str> = rows.iter().map(|row| row.username.as_str()).collect();

        assert_eq!(usernames, vec!["Alpha", "alpha2", "beta"]);
    }

    #[test]
    fn extract_online_members_preserves_rank_username_and_server_fields() {
        let payload = json!({
            "members": {
                "chief": {
                    "Obstacles_": { "online": true, "server": "NA5" }
                }
            }
        });

        let rows = extract_online_members(&payload);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        assert_eq!(row.rank_label, "Chief");
        assert_eq!(row.username, "Obstacles_");
        assert_eq!(row.server, "NA5");
    }
}
