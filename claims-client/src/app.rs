use std::collections::HashMap;

use leptos::prelude::*;
use sequoia_shared::TerritoryChange;

use crate::claims::ClaimsPage;

pub(crate) fn canvas_dimensions() -> (f64, f64) {
    let Some(window) = web_sys::window() else {
        return (1200.0, 800.0);
    };
    let w = window
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1200.0);
    let h = window
        .inner_height()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(800.0);
    (w, h)
}

pub(crate) fn set_loading_shell_step(step: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    if let Some(step_el) = document.get_element_by_id("app-loading-step") {
        step_el.set_text_content(Some(step));
    }
}

pub(crate) fn remove_loading_shell() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    if let Some(shell) = document.get_element_by_id("app-loading-shell") {
        shell.remove();
    }
}

pub(crate) const MOBILE_BREAKPOINT: f64 = 768.0;

#[derive(Clone, Copy)]
pub(crate) struct Hovered(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct Selected(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct AbbreviateNames(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowCountdown(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowGranularMapTime(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowCompoundMapTime(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowNames(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ThickCooldownBorders(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct BoldConnections(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ConnectionOpacityScale(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct ConnectionThicknessScale(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct ResourceHighlight(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowResourceIcons(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowTerritoryOrnaments(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct NameColorSetting(pub RwSignal<NameColor>);
#[derive(Clone, Copy)]
pub(crate) struct TagColorSetting(pub RwSignal<NameColor>);
#[derive(Clone, Copy)]
pub(crate) struct ReadableFont(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowMinimap(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct LabelScaleMaster(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct LabelScaleStatic(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct LabelScaleStaticName(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct LabelScaleDynamic(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct LabelScaleIcons(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarOpen(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarTransient(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowSettings(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct IsMobile(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct PeekTerritory(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct DetailReturnGuild(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct HeatModeEnabled(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatEntriesByTerritory(pub RwSignal<HashMap<String, u64>>);
#[derive(Clone, Copy)]
pub(crate) struct HeatMaxTakeCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct HeatWindowLabel(pub RwSignal<String>);
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapMode {
    Live,
    History,
}
#[derive(Clone, Copy)]
pub(crate) struct CurrentMode(pub RwSignal<MapMode>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryTimestamp(pub RwSignal<Option<i64>>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryFetchNonce(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct LastLiveSeq(pub RwSignal<Option<u64>>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBufferedUpdates(pub RwSignal<Vec<BufferedUpdate>>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBufferModeActive(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct NeedsLiveResync(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct LiveResyncInFlight(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct SseSeqGapDetectedCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBufferSizeMax(pub RwSignal<usize>);

#[derive(Clone, Debug)]
pub(crate) struct BufferedUpdate {
    pub seq: u64,
    pub changes: Vec<TerritoryChange>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum NameColor {
    White,
    Guild,
    Gold,
    Copper,
    Muted,
}

fn browser_pathname() -> String {
    web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_else(|| "/claims".to_string())
}

fn route_shell_title(path: &str) -> &'static str {
    match path.trim_end_matches('/') {
        "/claims/new/blank" => "Opening Blank Board",
        "/claims/new/live" => "Opening Live Snapshot",
        "/claims/new/draft" => "Recovering Draft",
        "/claims/new/import" => "Opening Imported Layout",
        path if path.starts_with("/claims/s/") => "Opening Saved Snapshot",
        _ => "Opening Claims Editor",
    }
}

#[component]
fn ClaimsEntryShell(title: &'static str) -> impl IntoView {
    view! {
        <div
            style="position: fixed; inset: 0; overflow: hidden; background:
                radial-gradient(circle at 18% 14%, rgba(52, 95, 182, 0.18), transparent 26%),
                radial-gradient(circle at 78% 12%, rgba(245, 197, 66, 0.12), transparent 24%),
                radial-gradient(circle at 50% 110%, rgba(17, 42, 86, 0.22), transparent 40%),
                linear-gradient(180deg, #060b14 0%, #03060d 100%);
                color: #eef3ff;"
        >
            <div
                style="position: absolute; inset: 0; background-image:
                    linear-gradient(rgba(78, 92, 125, 0.06) 1px, transparent 1px),
                    linear-gradient(90deg, rgba(78, 92, 125, 0.06) 1px, transparent 1px);
                    background-size: 56px 56px; mask-image: linear-gradient(180deg, rgba(255,255,255,0.5), transparent 88%);
                    pointer-events: none;"
            ></div>
            <div
                style="position: relative; min-height: 100vh; display: grid; place-items: center; padding: 32px;"
            >
                <div
                    style="width: min(560px, calc(100vw - 40px)); padding: 28px; border-radius: 28px;
                        border: 1px solid rgba(245, 197, 66, 0.18);
                        background: linear-gradient(180deg, rgba(16, 22, 34, 0.96), rgba(8, 13, 22, 0.94));
                        box-shadow: 0 28px 80px rgba(0, 0, 0, 0.42);"
                >
                    <div
                        style="display: inline-flex; align-items: center; gap: 10px; padding: 8px 12px;
                            border-radius: 999px; border: 1px solid rgba(245, 197, 66, 0.22);
                            background: rgba(245, 197, 66, 0.08); color: #f5c542; font-size: 0.7rem;
                            letter-spacing: 0.12em; text-transform: uppercase;"
                    >
                        "Claims Entry Shell"
                    </div>
                    <h1
                        style="margin: 18px 0 10px; font-family: 'Silkscreen', monospace; font-size: clamp(1.6rem, 5vw, 2.6rem);
                            line-height: 1; color: #f4c94b;"
                    >
                        {title}
                    </h1>
                    <p style="margin: 0; color: #9aa6c4; font-size: 0.82rem; line-height: 1.85;">
                        "Mounting the lightweight claims shell first so the editor can bootstrap route data without trapping the page behind the static HTML loader."
                    </p>
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn App() -> impl IntoView {
    let pathname = browser_pathname();
    let shell_ready = RwSignal::new(false);
    let title = route_shell_title(&pathname);
    let initial_path = pathname.clone();

    Effect::new(move || {
        set_loading_shell_step("Opening claims editor shell");
        remove_loading_shell();
        shell_ready.set(true);
    });

    view! {
        <div style="position: fixed; inset: 0;">
            {move || {
                if shell_ready.get() {
                    view! { <ClaimsPage initial_path=initial_path.clone() /> }.into_any()
                } else {
                    view! { <ClaimsEntryShell title=title /> }.into_any()
                }
            }}
        </div>
    }
}
