use js_sys::encode_uri_component;
use leptos::prelude::*;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_location, use_navigate};
use wasm_bindgen::JsCast;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const DEFAULT_SIDEBAR_WIDTH: f64 = 420.0;
pub(crate) const SIDEBAR_WIDTH_MIN: f64 = 320.0;
pub(crate) const SIDEBAR_WIDTH_MAX: f64 = 620.0;
const LEGACY_DEFAULT_SIDEBAR_WIDTH: f64 = 380.0;

pub(crate) fn clamp_sidebar_width(value: f64) -> f64 {
    value.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
}

pub(crate) const DEFAULT_WAR_PANEL_WIDTH: f64 = 300.0;
pub(crate) const WAR_PANEL_WIDTH_MIN: f64 = 220.0;
pub(crate) const WAR_PANEL_WIDTH_MAX: f64 = 460.0;

pub(crate) fn clamp_war_panel_width(value: f64) -> f64 {
    value.clamp(WAR_PANEL_WIDTH_MIN, WAR_PANEL_WIDTH_MAX)
}

pub(crate) fn migrate_sidebar_width(value: f64) -> f64 {
    if (value - LEGACY_DEFAULT_SIDEBAR_WIDTH).abs() < f64::EPSILON {
        DEFAULT_SIDEBAR_WIDTH
    } else {
        value
    }
}

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

struct TickIntervalBinding {
    window: web_sys::Window,
    interval_id: i32,
    _callback: wasm_bindgen::closure::Closure<dyn Fn()>,
}

struct KeydownBinding {
    window: web_sys::Window,
    _handler: wasm_bindgen::closure::Closure<dyn Fn(web_sys::KeyboardEvent)>,
}

struct ResizeBinding {
    window: web_sys::Window,
    _callback: wasm_bindgen::closure::Closure<dyn Fn()>,
}

thread_local! {
    static TICK_INTERVAL_BINDING: RefCell<Option<TickIntervalBinding>> = const { RefCell::new(None) };
    static KEYDOWN_BINDING: RefCell<Option<KeydownBinding>> = const { RefCell::new(None) };
    static RESIZE_BINDING: RefCell<Option<ResizeBinding>> = const { RefCell::new(None) };
}

use sequoia_shared::history::{
    HistoryGuildSrEntry, HistoryHeat, HistoryHeatMeta, HistoryHeatSource,
};
use sequoia_shared::{Region, Resources, SeasonScalarSample, TreasuryLevel, WarControllerState};

/// Newtype wrappers to give `hovered` and `selected` distinct types for Leptos context.
/// (Both are `RwSignal<Option<String>>` — without wrappers, `provide_context` overwrites one.)
#[derive(Clone, Copy)]
pub(crate) struct Hovered(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct Selected(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarOpen(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarWidth(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarTransient(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarIndex(pub RwSignal<usize>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarItems(pub RwSignal<Vec<String>>);
#[derive(Clone, Copy)]
pub(crate) struct ShowSettings(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct IsMobile(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct PeekTerritory(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct SelectedGuild(pub RwSignal<Option<String>>);
#[derive(Clone, Copy)]
pub(crate) struct DetailReturnGuild(pub RwSignal<Option<String>>);

#[derive(Clone, Default, PartialEq)]
pub(crate) struct GuildOnlineInfo {
    pub online: u32,
    pub season_rating: Option<i64>,
    pub season_rank: Option<u32>,
    pub season_id: Option<i32>,
    pub season_rating_source: Option<String>,
    pub season_rating_sampled_at: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct GuildOnlineData(pub RwSignal<HashMap<String, GuildOnlineInfo>>);
#[derive(Clone, Copy)]
pub(crate) struct LeaderboardSortBySr(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatEntriesByTerritory(pub RwSignal<HashMap<String, u64>>);
/// How heat mode ranks capture counts, for its legend.
#[derive(Clone, Copy)]
pub(crate) struct HeatScaleSignal(pub Signal<Arc<HeatScale>>);
#[derive(Clone, Copy)]
pub(crate) struct HeatFallbackApplied(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatWindowLabel(pub RwSignal<String>);
#[derive(Clone, Copy)]
pub(crate) struct HeatMetaState(pub RwSignal<Option<HistoryHeatMeta>>);

pub(crate) const MOBILE_BREAKPOINT: f64 = 768.0;
/// Gap below the minimap; in history mode it sits that far above the timeline.
const MINIMAP_LIVE_INSET: f32 = 16.0;
/// Gap below the phone's territory peek card: from the screen's edge, or from the History bar.
const PEEK_GAP_PX: f64 = 16.0;
/// Longest wait between website session probe attempts.
///
/// Doubling from a second, then held here for as long as the probe keeps failing. The loop
/// deliberately does not give up: it used to, after about half a minute, which is shorter than
/// a redeploy - and a member whose probe gave up kept the war feed hidden until the
/// [`VIEWER_PROBE_REFRESH`] sweep came round up to five minutes later, long after the server
/// had started serving them again. The server re-probes a stream every 30s
/// (`WAR_FEED_RETRY`); this is the display side of the same policy, at a comparable cadence.
const VIEWER_PROBE_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(16);
/// Wait before the first session probe retry, doubling towards [`VIEWER_PROBE_MAX_BACKOFF`].
const FIRST_PROBE_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);
/// How often an open map re-checks the session behind the war feed.
const VIEWER_PROBE_REFRESH: std::time::Duration = std::time::Duration::from_secs(300);
const GUILD_ONLINE_POLL_INTERVAL_SECS: u64 = 120;
const GUILD_ONLINE_BOOTSTRAP_RETRY_SECS: u64 = 3;
const GUILD_ONLINE_ERROR_RETRY_SECS: u64 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapMode {
    Live,
    History,
}

fn map_mode_from_path(path: &str) -> MapMode {
    if path == "/history" || path.starts_with("/history/") {
        MapMode::History
    } else {
        MapMode::Live
    }
}

fn canonical_path_for_mode(mode: MapMode) -> &'static str {
    match mode {
        MapMode::Live => "/",
        MapMode::History => "/history",
    }
}

fn should_wait_for_history_probe(history_is_available: bool, history_probe_nonce: u64) -> bool {
    !history_is_available && history_probe_nonce == 0
}

#[derive(Clone, Copy)]
pub(crate) struct CurrentMode(pub RwSignal<MapMode>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryTimestamp(pub RwSignal<Option<i64>>);
/// The instant the drawn territories show in History: the requested time only once its state
/// is on the map. `None` while the live map is still drawn.
#[derive(Clone, Copy)]
pub(crate) struct HistoryMapTime(pub RwSignal<Option<i64>>);
/// The History bar's measured height in CSS pixels; 0 until it has been laid out.
#[derive(Clone, Copy)]
pub(crate) struct HistoryBarHeight(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct PlaybackActive(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct PlaybackSpeed(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBoundsSignal(pub RwSignal<Option<(i64, i64)>>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryAvailable(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryLegacyGeometryActive(pub RwSignal<bool>);
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
pub(crate) struct LiveHandoffResyncCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct LiveSeasonScalarSample(pub RwSignal<Option<SeasonScalarSample>>);
#[derive(Clone, Copy)]
pub(crate) struct HistorySeasonScalarSample(pub RwSignal<Option<SeasonScalarSample>>);
#[derive(Clone, Copy)]
pub(crate) struct HistorySeasonLeaderboard(pub RwSignal<Option<Vec<HistoryGuildSrEntry>>>);
#[derive(Clone, Copy)]
pub(crate) struct WarControllerData(pub RwSignal<Option<WarControllerState>>);
/// Whether the viewer may see war features at all: a Sequoia member ranked Sprite or above, or
/// a website admin, as the server says on `/api/auth/me`.
///
/// The server is the real gate - it refuses `/api/warcontroller` and withholds the SSE
/// `warcontroller` frames from everyone else. This is the display side of the same rule, which
/// [`crate::war_room::WarRoom`] applies to everything war-related on the map and in the HUD.
#[derive(Clone, Copy)]
pub(crate) struct WarFeedVisible(pub Memo<bool>);
/// Whether the war panel is expanded.
#[derive(Clone, Copy)]
pub(crate) struct WarPanelOpen(pub RwSignal<bool>);
/// Width of the war panel, drag-resizable like the sidebar's.
#[derive(Clone, Copy)]
pub(crate) struct WarPanelWidth(pub RwSignal<f64>);

pub(crate) use sequoia_map_engine::territory::BufferedUpdate;

pub(crate) type TerritoryGeometry = (Region, Resources, Vec<String>);
pub(crate) type TerritoryGeometryMap = HashMap<String, TerritoryGeometry>;
pub(crate) type GuildColorMap = HashMap<String, (u8, u8, u8)>;

/// Immutable snapshot of territory geometry captured when entering history mode.
/// Prevents history/playback operations from degrading geometry data.
#[derive(Clone, Copy)]
pub(crate) struct TerritoryGeometryStore(pub StoredValue<TerritoryGeometryMap>);
#[derive(Clone, Copy)]
pub(crate) struct GuildColorStore(pub StoredValue<GuildColorMap>);

/// Whether the system asks for reduced motion, following changes while the page is open.
fn prefers_reduced_motion() -> Signal<bool> {
    let query = web_sys::window()
        .and_then(|window| window.match_media("(prefers-reduced-motion: reduce)").ok())
        .flatten();
    let Some(query) = query else {
        return Signal::stored(false);
    };
    let reduced = RwSignal::new(query.matches());
    let changed = wasm_bindgen::closure::Closure::<dyn Fn(web_sys::MediaQueryListEvent)>::new(
        move |event: web_sys::MediaQueryListEvent| reduced.set(event.matches()),
    );
    let _ = query.add_event_listener_with_callback("change", changed.as_ref().unchecked_ref());
    // The app lives as long as the page, and so does the listener.
    changed.forget();
    reduced.into()
}

use crate::colors::rgba_css;
use crate::heat::{self, HeatFetchInput};
use crate::history;
use crate::keybinds::{KeybindHelpOpen, Shortcut};
use crate::map_intel::MapIntelOverlay;
use crate::navbar::{CurrentViewer, SiteNavbar};
use crate::season_scalar;
use crate::settings::{self, Layout, Overlay, ViewerSettings};
use crate::sidebar::Sidebar;
use crate::territory::{ClientTerritoryMap, from_snapshot};
use crate::territory_card::{
    DefenseValue, ResourceRow, ResourceTable, TerritoryHead, TerritoryWar, TreasuryValue, cooldown,
    territory_resource_rows, treasury,
};
use crate::time_format::format_hms;
use crate::timeline::Timeline;
use crate::viewport::Viewport;
use crate::war_room::{self, WarRoom, WarRoomInputs};
use sequoia_browser_map::live_feed::{self, LiveFeed};
use sequoia_browser_map::{BrowserMap, HeatLayer, MapCanvas, MapEvent, MapInputs};
use sequoia_map_engine::heat::HeatScale;
use sequoia_map_engine::settings::{LabelScales, RenderSettings};

fn format_heat_window_time(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.format("%b %d").to_string())
        .unwrap_or_else(|_| raw.to_string())
}

fn format_heat_window_label(heat: &HistoryHeat, mode: MapMode) -> String {
    let source = match (heat.source, heat.season_id) {
        (HistoryHeatSource::Season, Some(season_id)) => format!("Season {season_id}"),
        (HistoryHeatSource::Season, None) => "Season (fallback)".to_string(),
        (HistoryHeatSource::AllTime, _) => "All-time".to_string(),
    };
    let mode_label = if mode == MapMode::History {
        "cumulative"
    } else {
        "totals"
    };
    format!(
        "{source} {mode_label} • {} → {}",
        format_heat_window_time(&heat.from),
        format_heat_window_time(&heat.to)
    )
}

/// `Some(false)` for Ctrl+Z (Cmd+Z), `Some(true)` for Ctrl+Shift+Z or Ctrl+Y.
fn settings_undo_key(e: &web_sys::KeyboardEvent) -> Option<bool> {
    if !(e.ctrl_key() || e.meta_key()) || e.alt_key() {
        return None;
    }
    match e.key().to_ascii_lowercase().as_str() {
        "z" => Some(e.shift_key()),
        "y" if !e.shift_key() => Some(true),
        _ => None,
    }
}

/// Whether the key goes to a field the viewer types into.
fn edits_text(target: Option<web_sys::EventTarget>) -> bool {
    let Some(element) = target.and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    if element.is_content_editable() || element.tag_name() == "TEXTAREA" {
        return true;
    }
    element
        .dyn_into::<web_sys::HtmlInputElement>()
        .is_ok_and(|input| {
            !matches!(
                input.type_().as_str(),
                "range" | "radio" | "checkbox" | "button" | "submit" | "reset" | "color"
            )
        })
}

/// The saved season, or Latest (`None`, which the server reads as the newest season and so
/// follows a new one) when the server no longer has the season saved.
fn normalize_heat_selected_season_id(meta: &HistoryHeatMeta, selected: Option<i32>) -> Option<i32> {
    selected.filter(|season_id| {
        meta.seasons
            .iter()
            .any(|window| window.season_id == *season_id)
    })
}
#[component]
pub fn App() -> impl IntoView {
    view! { <MapPage /> }
}

#[component]
pub fn MapPage() -> impl IntoView {
    let location = use_location();
    let navigate = use_navigate();
    // Global signals
    let territories: RwSignal<ClientTerritoryMap> = RwSignal::new(Default::default());
    let selected: RwSignal<Option<String>> = RwSignal::new(None);
    let search_query: RwSignal<String> = RwSignal::new(String::new());
    // Epoch-second tick — drives cooldown countdown updates across canvas, tooltip, sidebar
    let tick: RwSignal<i64> = RwSignal::new(chrono::Utc::now().timestamp());
    let (settings, layout) = ViewerSettings::load();
    let heat_mode_enabled = settings.value(settings::HEAT_MAP);
    let show_minimap = settings.value(settings::MINIMAP);
    let heat_selected_season_id = settings.value(settings::HEAT_SEASON);
    let heat_live_season = settings.value(settings::HEAT_LIVE_SEASON);
    let heat_history_season = settings.value(settings::HEAT_HISTORY_SEASON);
    let reduced_motion = prefers_reduced_motion();
    let sidebar_width: RwSignal<f64> = RwSignal::new(layout.sidebar_width);
    let sidebar_open: RwSignal<bool> = RwSignal::new(layout.sidebar_open);
    let war_panel_open: RwSignal<bool> = RwSignal::new(layout.war_panel_open);
    let war_panel_width: RwSignal<f64> = RwSignal::new(layout.war_panel_width);
    let sidebar_transient: RwSignal<bool> = RwSignal::new(false);
    let sidebar_ready: RwSignal<bool> = RwSignal::new(false);
    let sidebar_loaded: RwSignal<bool> = RwSignal::new(layout.sidebar_open);
    let sidebar_index: RwSignal<usize> = RwSignal::new(0);
    let sidebar_items: RwSignal<Vec<String>> = RwSignal::new(Vec::new());
    let keybind_help_open: RwSignal<bool> = RwSignal::new(false);
    let show_settings: RwSignal<bool> = RwSignal::new(false);
    // Live-first boot: defer non-essential work (tiles/history checks/icons)
    // until we have initial territory data and the first frame has had a chance to commit.
    let deferred_boot_ready: RwSignal<bool> = RwSignal::new(false);
    let deferred_boot_timer_set: RwSignal<bool> = RwSignal::new(false);
    let live_bootstrap_started: RwSignal<bool> = RwSignal::new(false);
    let tile_fetch_scheduled: RwSignal<bool> = RwSignal::new(false);
    let loading_shell_removed: RwSignal<bool> = RwSignal::new(false);

    // Mobile detection
    let is_mobile: RwSignal<bool> = RwSignal::new(canvas_dimensions().0 < MOBILE_BREAKPOINT);
    let peek_territory: RwSignal<Option<String>> = RwSignal::new(None);
    let selected_guild: RwSignal<Option<String>> = RwSignal::new(None);
    let detail_return_guild: RwSignal<Option<String>> = RwSignal::new(None);
    let guild_online_data: RwSignal<HashMap<String, GuildOnlineInfo>> =
        RwSignal::new(HashMap::new());
    let leaderboard_sort_by_sr: RwSignal<bool> = RwSignal::new(layout.leaderboard_sort_by_sr);
    let heat_entries_by_territory: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    let heat_fallback_applied: RwSignal<bool> = RwSignal::new(false);
    let heat_window_label: RwSignal<String> = RwSignal::new(String::new());
    let heat_meta: RwSignal<Option<HistoryHeatMeta>> = RwSignal::new(None);
    let heat_refresh_nonce: RwSignal<u64> = RwSignal::new(0);

    // History mode signals
    let map_mode: RwSignal<MapMode> = RwSignal::new(MapMode::Live);
    let history_timestamp: RwSignal<Option<i64>> = RwSignal::new(None);
    let history_map_time: RwSignal<Option<i64>> = RwSignal::new(None);
    let history_bar_height: RwSignal<f64> = RwSignal::new(0.0);
    Effect::new(move || {
        map_mode.track();
        history_map_time.set(None);
    });
    let playback_active: RwSignal<bool> = RwSignal::new(false);
    let playback_speed: RwSignal<f64> = RwSignal::new(10.0);
    let history_bounds: RwSignal<Option<(i64, i64)>> = RwSignal::new(None);
    let history_available: RwSignal<bool> = RwSignal::new(false);
    let history_legacy_geometry_active: RwSignal<bool> = RwSignal::new(false);
    let history_probe_nonce: RwSignal<u64> = RwSignal::new(0);
    let history_fetch_nonce: RwSignal<u64> = RwSignal::new(0);
    let live_handoff_resync_count: RwSignal<u64> = RwSignal::new(0);
    let live_season_scalar_sample: RwSignal<Option<SeasonScalarSample>> = RwSignal::new(None);
    let history_season_scalar_sample: RwSignal<Option<SeasonScalarSample>> = RwSignal::new(None);
    let history_season_leaderboard: RwSignal<Option<Vec<HistoryGuildSrEntry>>> =
        RwSignal::new(None);
    let route_mode_sync_in_flight: RwSignal<bool> = RwSignal::new(false);
    let warcontroller_state: RwSignal<Option<WarControllerState>> = RwSignal::new(None);
    // Website session, resolved on mount below. Declared here because the war feed is
    // gated on it: the server refuses that feed to anyone without war access, and the map
    // must not render it - or reserve space for it - either.
    let viewer = RwSignal::new(None::<crate::auth::Viewer>);
    // The server's answer, kept as it was while the server cannot tell.
    let war_access = RwSignal::new(false);
    let war_feed_visible: Memo<bool> = Memo::new(move |_| war_access.get());
    let account = Memo::new(move |_| {
        viewer.with(|viewer| {
            viewer
                .as_ref()
                .map(|viewer| (viewer.discord_id.clone(), viewer.minecraft_uuid.clone()))
        })
    });
    let territory_geometry: StoredValue<TerritoryGeometryMap> = StoredValue::new(HashMap::new());
    let guild_colors: StoredValue<GuildColorMap> = StoredValue::new(HashMap::new());

    let feed = LiveFeed {
        war_controller: Some(warcontroller_state),
        ..LiveFeed::new(
            territories,
            Signal::derive(move || map_mode.get() == MapMode::History),
        )
    };
    let LiveFeed {
        connection,
        last_live_seq,
        buffered_updates: history_buffered_updates,
        buffer_mode_active: history_buffer_mode_active,
        needs_resync: needs_live_resync,
        ..
    } = feed;
    let war_room = WarRoom::new(WarRoomInputs {
        feed: warcontroller_state,
        visible: Signal::derive(move || war_feed_visible.get() && map_mode.get() == MapMode::Live),
        territories,
        connection,
        tick,
    });

    let render_settings = Memo::new(move |_| {
        // History's clock can stand still or race; the urgency of a cooldown ending is live.
        let cooldown_pulse = map_mode.get() == MapMode::Live && !reduced_motion.get();
        settings.with(|s| RenderSettings {
            thick_cooldown_borders: s.thick_cooldown_borders,
            suppress_cooldown_visuals: false,
            cooldown_display: s.cooldown_display,
            cooldown_pulse,
            resource_highlight: s.overlay == Overlay::Resources,
            resource_highlight_opacity: s.resource_highlight_opacity as f32,
            defense_highlight: s.overlay == Overlay::Defense,
            fill_alpha_boost: 0.0,
            territory_opacity: s.territory_fill_opacity as f32,
            show_connections: s.show_connections,
            connection_style: s.connection_style,
            bold_connections: s.bold_connections,
            connection_opacity_scale: s.connection_opacity_scale as f32,
            connection_solid_opacity: s.connection_solid_opacity as f32,
            connection_thickness_scale: s.connection_thickness_scale as f32,
            connection_zoom_fade: (0.15, 0.45),
            show_names: s.show_names,
            abbreviate_names: s.abbreviate_names,
            show_claim_labels: s.show_claim_labels,
            show_far_zoom_territory_tags: s.show_far_zoom_territory_tags,
            name_color: s.name_color,
            tag_color: s.tag_color,
            readable_font: s.readable_font,
            show_countdown: s.show_countdown,
            granular_map_time: s.granular_map_time,
            compound_map_time: s.compound_map_time,
            show_resource_icons: s.show_resource_icons,
            show_territory_ornaments: s.show_territory_ornaments,
            hq_crown_style: s.hq_crown_style,
            hq_crown_scale: s.hq_crown_scale as f32,
            war_outline: s.war_outline,
            label_scales: LabelScales {
                master: s.label_scale_master as f32,
                static_tag: s.label_scale_static as f32,
                static_name: s.label_scale_static_name as f32,
                dynamic: s.label_scale_dynamic as f32,
                icons: s.label_scale_icons as f32,
            },
        })
    });
    // Timers count against the time of the territories drawn, not a seek still loading.
    let map_clock = Memo::new(move |_| match map_mode.get() {
        MapMode::History => history_map_time.get().unwrap_or_else(|| tick.get()),
        MapMode::Live => tick.get(),
    });
    let minimap_inset = Memo::new(move |_| {
        (!is_mobile.get() && show_minimap.get()).then_some(match map_mode.get() {
            MapMode::History => history_bar_height.get() as f32 + MINIMAP_LIVE_INSET,
            MapMode::Live => MINIMAP_LIVE_INSET,
        })
    });
    let heat_layer = HeatLayer::new(
        heat_mode_enabled.into(),
        heat_entries_by_territory.into(),
        territories.into(),
    );
    let map = BrowserMap::new(
        MapInputs {
            territories: territories.into(),
            // The peeked territory stays outlined while its card is open.
            selected: Signal::derive(move || selected.get().or_else(|| peek_territory.get())),
            settings: render_settings.into(),
            clock_secs: map_clock.into(),
            // Paused history holds `map_clock` still; cooldown pulses keep animating.
            animation_tick: Some(tick.into()),
            heat: Some(heat_layer),
            war_marks: Some(war_room.marks().into()),
            minimap_inset: minimap_inset.into(),
            edit: Signal::stored(Default::default()),
        },
        Viewport::default(),
    );
    let camera = map.camera();
    let hovered = map.hovered();

    // Provide via context so children can access
    provide_context(territories);
    provide_context(map);
    provide_context(Hovered(hovered));
    provide_context(Selected(selected));
    provide_context(search_query);
    provide_context(connection);
    provide_context(tick);
    provide_context(KeybindHelpOpen(keybind_help_open));
    provide_context(SidebarOpen(sidebar_open));
    provide_context(WarPanelOpen(war_panel_open));
    provide_context(WarPanelWidth(war_panel_width));
    provide_context(SidebarWidth(sidebar_width));
    provide_context(SidebarTransient(sidebar_transient));
    provide_context(SidebarIndex(sidebar_index));
    provide_context(SidebarItems(sidebar_items));
    provide_context(settings);
    provide_context(ShowSettings(show_settings));
    provide_context(CurrentMode(map_mode));
    provide_context(HistoryTimestamp(history_timestamp));
    provide_context(HistoryMapTime(history_map_time));
    provide_context(HistoryBarHeight(history_bar_height));
    provide_context(PlaybackActive(playback_active));
    provide_context(PlaybackSpeed(playback_speed));
    provide_context(HistoryBoundsSignal(history_bounds));
    provide_context(HistoryAvailable(history_available));
    provide_context(HistoryLegacyGeometryActive(history_legacy_geometry_active));
    provide_context(HistoryFetchNonce(history_fetch_nonce));
    provide_context(LastLiveSeq(last_live_seq));
    provide_context(HistoryBufferedUpdates(history_buffered_updates));
    provide_context(HistoryBufferModeActive(history_buffer_mode_active));
    provide_context(NeedsLiveResync(needs_live_resync));
    provide_context(LiveHandoffResyncCount(live_handoff_resync_count));
    provide_context(LiveSeasonScalarSample(live_season_scalar_sample));
    provide_context(WarControllerData(warcontroller_state));
    provide_context(war_room);
    provide_context(WarFeedVisible(war_feed_visible));
    provide_context(HistorySeasonScalarSample(history_season_scalar_sample));
    provide_context(HistorySeasonLeaderboard(history_season_leaderboard));
    provide_context(TerritoryGeometryStore(territory_geometry));
    provide_context(GuildColorStore(guild_colors));
    provide_context(crate::tower::TowerState::new());
    provide_context(IsMobile(is_mobile));

    // Website session, resolved on mount (the signal itself is declared alongside the war
    // feed above). The navbar shows who is signed in, and the war feed is gated on the
    // session's war access, so a failure here leaves the bar reading "Sign in" and the war
    // overview hidden - which is the safe way round for internal data.
    provide_context(CurrentViewer(viewer));
    provide_context(crate::member_war::MemberWar::new());
    // The retry loop below never terminates on its own, so a second caller - `pageshow` after
    // a back-forward restore, while the first is still backing off - would leave two loops
    // probing forever. One at a time.
    let probe_in_flight = RwSignal::new(false);
    let apply_session = move |session: crate::auth::Session| {
        let held = viewer.get_untracked();
        if !crate::auth::same_account(held.as_ref(), session.viewer.as_ref()) {
            // Another account's war state must not stay on screen for this one; the seed below
            // asks again for the new account if it may see any.
            warcontroller_state.set(None);
        }
        war_access.set(session.war_access_after(held.as_ref(), war_access.get_untracked()));
        viewer.set(session.viewer);
    };
    let refresh_viewer = move || {
        if probe_in_flight.get_untracked() {
            return;
        }
        probe_in_flight.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            // Retried on failure, and only on failure: a signed-out answer is final, but a
            // transport blip used to read the same way and left a member without the war feed
            // for the rest of the page's life. An existing viewer is kept while retrying, so a
            // hiccup never blanks a session that is still good.
            let mut backoff = FIRST_PROBE_BACKOFF;
            loop {
                match crate::auth::fetch_session().await {
                    Ok(session) => {
                        apply_session(session);
                        probe_in_flight.set(false);
                        return;
                    }
                    Err(error) => {
                        // Once per outage, not once per attempt: this loop runs until it
                        // succeeds, and a backend that stays down would otherwise fill the
                        // console every few seconds for the life of the page.
                        if backoff == FIRST_PROBE_BACKOFF {
                            web_sys::console::warn_1(
                                &format!("website session probe failed, retrying: {error}").into(),
                            );
                        }
                        gloo_timers::future::sleep(backoff).await;
                        backoff = (backoff * 2).min(VIEWER_PROBE_MAX_BACKOFF);
                    }
                }
            }
        });
    };
    refresh_viewer();
    // Membership and rank can change, and a session can expire, while the map stays open for
    // hours.
    // The server mutes the war feed on its own schedule; this is the display side of that, so
    // the panel and the at-war highlight cannot outlive the access they depend on.
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(VIEWER_PROBE_REFRESH).await;
            if let Ok(session) = crate::auth::fetch_session().await {
                apply_session(session);
            }
        }
    });
    // Signing in and out are full page loads, so the mount above normally sees
    // the new session - except on a back-forward cache restore, which replays
    // the old DOM without re-running any of this. Re-probe on a persisted
    // `pageshow` so the chip cannot outlive the session it describes.
    {
        let handler =
            wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::PageTransitionEvent)>::new(
                move |event: web_sys::PageTransitionEvent| {
                    if event.persisted() {
                        refresh_viewer();
                    }
                },
            );
        if let Some(window) = web_sys::window() {
            let _ = window.add_event_listener_with_callback(
                "pageshow",
                wasm_bindgen::JsCast::unchecked_ref(handler.as_ref()),
            );
        }
        // The listener lives as long as the page does; the map is never
        // unmounted, so leaking it is the correct lifetime here.
        handler.forget();
    }

    provide_context(PeekTerritory(peek_territory));
    provide_context(SelectedGuild(selected_guild));
    provide_context(DetailReturnGuild(detail_return_guild));
    provide_context(GuildOnlineData(guild_online_data));
    provide_context(LeaderboardSortBySr(leaderboard_sort_by_sr));
    provide_context(HeatEntriesByTerritory(heat_entries_by_territory));
    provide_context(HeatScaleSignal(heat_layer.scale()));
    provide_context(HeatFallbackApplied(heat_fallback_applied));
    provide_context(HeatWindowLabel(heat_window_label));
    provide_context(HeatMetaState(heat_meta));

    // Closing the mobile sheet on a territory's details falls back to its peek card.
    {
        let sheet_was_open = Cell::new(sidebar_open.get_untracked());
        Effect::new(move || {
            let open = sidebar_open.get();
            if sheet_was_open.replace(open)
                && !open
                && is_mobile.get_untracked()
                && !show_settings.get_untracked()
                && let Some(territory) = selected.get_untracked()
            {
                selected.set(None);
                detail_return_guild.set(None);
                peek_territory.set(Some(territory));
            }
        });
    }

    // Mutual exclusion: SelectedGuild and Selected clear each other
    Effect::new(move || {
        if selected_guild.get().is_some() {
            selected.set(None);
        }
    });
    Effect::new(move || {
        if selected.get().is_some() {
            selected_guild.set(None);
        }
    });

    // Probe history capability once on startup so the History toggle appears automatically.
    history::check_availability_with_probe(history_available, Some(history_probe_nonce));

    // Keep URL path and map mode in sync without remounting the app.
    Effect::new({
        let navigate = navigate.clone();
        move || {
            let path = location.pathname.get();
            let target_mode = map_mode_from_path(&path);
            let current_mode = map_mode.get();
            let history_is_available = history_available.get();
            let probe_nonce = history_probe_nonce.get();

            if target_mode == current_mode {
                if route_mode_sync_in_flight.get() {
                    route_mode_sync_in_flight.set(false);
                }
                return;
            }

            match target_mode {
                MapMode::History => {
                    if !history_is_available {
                        // History capability may become available shortly after startup.
                        history::check_availability_with_probe(
                            history_available,
                            Some(history_probe_nonce),
                        );
                        if should_wait_for_history_probe(history_is_available, probe_nonce) {
                            return;
                        }

                        // A completed probe still reports unavailable history: canonicalize to
                        // live mode instead of keeping a stale /history URL.
                        route_mode_sync_in_flight.set(true);
                        navigate(
                            "/",
                            NavigateOptions {
                                replace: true,
                                ..Default::default()
                            },
                        );
                        return;
                    }
                    route_mode_sync_in_flight.set(true);
                    history::enter_history_mode(history::EnterHistoryModeInput {
                        history_map_time,
                        mode: map_mode,
                        history_timestamp,
                        history_bounds,
                        history_fetch_nonce,
                        history_legacy_geometry_active,
                        history_buffered_updates,
                        history_buffer_mode_active,
                        needs_live_resync,
                        history_scalar_sample: history_season_scalar_sample,
                        history_sr_leaderboard: history_season_leaderboard,
                        geo_store: territory_geometry,
                        guild_color_store: guild_colors,
                        territories,
                    });
                }
                MapMode::Live => {
                    route_mode_sync_in_flight.set(true);
                    history::exit_history_mode(history::ExitHistoryModeInput {
                        mode: map_mode,
                        playback_active,
                        history_fetch_nonce,
                        history_timestamp,
                        history_legacy_geometry_active,
                        history_buffered_updates,
                        history_buffer_mode_active,
                        last_live_seq,
                        needs_live_resync,
                        live_handoff_resync_count,
                        history_sr_leaderboard: history_season_leaderboard,
                        territories,
                    });
                }
            }
        }
    });

    Effect::new({
        move || {
            let mode_now = map_mode.get();
            let path_now = location.pathname.get();

            if route_mode_sync_in_flight.get() {
                if map_mode_from_path(&path_now) == mode_now {
                    route_mode_sync_in_flight.set(false);
                }
                return;
            }

            let target_path = canonical_path_for_mode(mode_now);
            if path_now != target_path {
                navigate(
                    target_path,
                    NavigateOptions {
                        replace: true,
                        ..Default::default()
                    },
                );
            }
        }
    });

    // Reset history scalar snapshot when returning to live mode.
    Effect::new(move || {
        if map_mode.get() == MapMode::Live {
            history_season_scalar_sample.set(None);
            history_season_leaderboard.set(None);
        }
    });

    let request_heat_refresh: Rc<dyn Fn(Option<i64>)> = Rc::new({
        move |at_override: Option<i64>| {
            if !heat_mode_enabled.get_untracked() {
                return;
            }

            let mode_now = map_mode.get_untracked();
            let (season_wide, at) = if mode_now == MapMode::History {
                (
                    heat_history_season.get_untracked(),
                    at_override.or(history_timestamp.get_untracked()),
                )
            } else {
                (heat_live_season.get_untracked(), None)
            };
            let source = if season_wide {
                HistoryHeatSource::Season
            } else {
                HistoryHeatSource::AllTime
            };

            let season_id = if source == HistoryHeatSource::Season {
                heat_selected_season_id.get_untracked()
            } else {
                None
            };

            let request_nonce = heat_refresh_nonce.get_untracked().wrapping_add(1);
            heat_refresh_nonce.set(request_nonce);
            wasm_bindgen_futures::spawn_local(async move {
                match heat::fetch_heat(HeatFetchInput {
                    source,
                    season_id,
                    at,
                })
                .await
                {
                    Ok(payload) => {
                        if heat_refresh_nonce.get_untracked() != request_nonce {
                            return;
                        }
                        let entries_map: HashMap<String, u64> = payload
                            .entries
                            .iter()
                            .map(|entry| (entry.territory.clone(), entry.take_count))
                            .collect();
                        heat_entries_by_territory.set(entries_map);
                        heat_fallback_applied.set(payload.fallback_applied);
                        heat_window_label.set(format_heat_window_label(&payload, mode_now));
                    }
                    Err(e) => {
                        if heat_refresh_nonce.get_untracked() != request_nonce {
                            return;
                        }
                        web_sys::console::warn_1(&format!("heat fetch failed: {e}").into());
                        heat_entries_by_territory.set(HashMap::new());
                        heat_fallback_applied.set(false);
                        heat_window_label.set(String::new());
                    }
                }
            });
        }
    });
    // Once the server's seasons are known, a saved season it no longer has becomes Latest, in
    // the settings and everywhere Undo and Redo can reach.
    Effect::new(move || {
        heat_meta.with(|meta| {
            if let Some(meta) = meta {
                settings.correct(|s| {
                    s.heat_selected_season_id =
                        normalize_heat_selected_season_id(meta, s.heat_selected_season_id);
                });
            }
        });
    });
    // History heat fetch throttling state (leading + trailing calls).
    let heat_history_refresh_timeout =
        Rc::new(RefCell::new(None::<gloo_timers::callback::Timeout>));
    let heat_history_pending_at = Rc::new(Cell::new(None::<i64>));
    let heat_history_last_refresh_at_ms = Rc::new(Cell::new(0.0));

    Effect::new({
        let request_heat_refresh = Rc::clone(&request_heat_refresh);
        move || {
            if !heat_mode_enabled.get() {
                heat_entries_by_territory.set(HashMap::new());
                heat_fallback_applied.set(false);
                heat_window_label.set(String::new());
                return;
            }
            if heat_meta.get().is_some() {
                return;
            }
            let request_heat_refresh = Rc::clone(&request_heat_refresh);
            wasm_bindgen_futures::spawn_local(async move {
                match heat::fetch_heat_meta().await {
                    Ok(meta) => {
                        let selected = heat_selected_season_id.get_untracked();
                        heat_meta.set(Some(meta));
                        // A season corrected below refreshes as it changes; Latest asks now.
                        if selected.is_none() {
                            request_heat_refresh(None);
                        }
                    }
                    Err(e) => {
                        web_sys::console::warn_1(&format!("heat meta fetch failed: {e}").into());
                    }
                }
            });
        }
    });

    Effect::new({
        let request_heat_refresh = Rc::clone(&request_heat_refresh);
        let heat_history_refresh_timeout = Rc::clone(&heat_history_refresh_timeout);
        let heat_history_pending_at = Rc::clone(&heat_history_pending_at);
        let heat_history_last_refresh_at_ms = Rc::clone(&heat_history_last_refresh_at_ms);
        move || {
            if !heat_mode_enabled.get() {
                if let Some(timeout) = heat_history_refresh_timeout.borrow_mut().take() {
                    timeout.cancel();
                }
                heat_history_pending_at.set(None);
                return;
            }

            let mode_now = map_mode.get();
            heat_live_season.track();
            heat_history_season.track();
            heat_selected_season_id.track();
            let history_at = history_timestamp.get();

            if mode_now == MapMode::History {
                let now_ms = js_sys::Date::now();
                let elapsed_ms = now_ms - heat_history_last_refresh_at_ms.get();
                if elapsed_ms >= 120.0 && heat_history_refresh_timeout.borrow().is_none() {
                    heat_history_last_refresh_at_ms.set(now_ms);
                    request_heat_refresh(history_at);
                    return;
                }

                heat_history_pending_at.set(history_at);
                if heat_history_refresh_timeout.borrow().is_some() {
                    return;
                }

                let wait_ms = (120.0 - elapsed_ms).max(0.0).round() as u32;
                let request_heat_refresh = Rc::clone(&request_heat_refresh);
                let heat_history_refresh_timeout_cb = Rc::clone(&heat_history_refresh_timeout);
                let heat_history_pending_at_cb = Rc::clone(&heat_history_pending_at);
                let heat_history_last_refresh_at_ms_cb =
                    Rc::clone(&heat_history_last_refresh_at_ms);
                let timeout = gloo_timers::callback::Timeout::new(wait_ms, move || {
                    let _ = heat_history_refresh_timeout_cb.borrow_mut().take();
                    let pending_at = heat_history_pending_at_cb.take();
                    heat_history_last_refresh_at_ms_cb.set(js_sys::Date::now());
                    request_heat_refresh(pending_at);
                });
                *heat_history_refresh_timeout.borrow_mut() = Some(timeout);
            } else {
                if let Some(timeout) = heat_history_refresh_timeout.borrow_mut().take() {
                    timeout.cancel();
                }
                heat_history_pending_at.set(None);
                heat_history_last_refresh_at_ms.set(0.0);
                request_heat_refresh(None);
            }
        }
    });

    wasm_bindgen_futures::spawn_local({
        let request_heat_refresh = Rc::clone(&request_heat_refresh);
        async move {
            loop {
                if heat_mode_enabled.get_untracked() && map_mode.get_untracked() == MapMode::Live {
                    request_heat_refresh(None);
                }
                gloo_timers::future::sleep(std::time::Duration::from_secs(60)).await;
            }
        }
    });

    // Seed war controller state once the session has war access; the SSE stream keeps it
    // current afterwards. Fetching before that would be a guaranteed 403, and anything
    // already held has to go the moment access lapses.
    Effect::new(move |_| {
        account.track();
        if !war_feed_visible.get() {
            if warcontroller_state.with_untracked(Option::is_some) {
                warcontroller_state.set(None);
            }
            return;
        }
        // What the feed held when the request went out: anything the stream delivers while
        // it is in flight is at least as current as the answer, which then only fills in.
        let held = warcontroller_state.get_untracked();
        wasm_bindgen_futures::spawn_local(async move {
            match war_room::fetch_warcontroller().await {
                Ok(state) => warcontroller_state.maybe_update(|current| {
                    if *current != held || !state.supersedes(current.as_ref()) {
                        return false;
                    }
                    *current = Some(state);
                    true
                }),
                Err(e) => {
                    web_sys::console::warn_1(&format!("war controller fetch failed: {e}").into());
                }
            }
        });
    });

    // Poll shared server-side scalar estimate while in live mode.
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            if map_mode.get_untracked() == MapMode::Live
                && settings.get_untracked(settings::ESTIMATE_SR_SCALAR)
            {
                match season_scalar::fetch_current_scalar_sample().await {
                    Ok(sample) => live_season_scalar_sample.set(sample),
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("season scalar fetch failed: {e}").into(),
                        );
                    }
                }
            }
            gloo_timers::future::sleep(std::time::Duration::from_secs(60)).await;
        }
    });

    // Poll guild online data (online counts + season ratings) while in live mode.
    wasm_bindgen_futures::spawn_local(async move {
        fn parse_online_response(
            raw: HashMap<String, serde_json::Value>,
        ) -> HashMap<String, GuildOnlineInfo> {
            raw.into_iter()
                .filter_map(|(name, val)| {
                    let info = if let Some(obj) = val.as_object() {
                        // New format: { "online": 42, "season_rating": 12000 }
                        GuildOnlineInfo {
                            online: obj.get("online").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                            season_rating: obj.get("season_rating").and_then(|v| v.as_i64()),
                            season_rank: obj
                                .get("season_rank")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u32::try_from(v).ok()),
                            season_id: obj
                                .get("season_id")
                                .and_then(|v| v.as_i64())
                                .and_then(|v| i32::try_from(v).ok()),
                            season_rating_source: obj
                                .get("season_rating_source")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            season_rating_sampled_at: obj
                                .get("season_rating_sampled_at")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        }
                    } else if let Some(n) = val.as_u64() {
                        // Legacy format: plain u32
                        GuildOnlineInfo {
                            online: n as u32,
                            season_rating: None,
                            season_rank: None,
                            season_id: None,
                            season_rating_source: None,
                            season_rating_sampled_at: None,
                        }
                    } else {
                        return None;
                    };
                    Some((name, info))
                })
                .collect()
        }

        fn encoded_names_query(names: impl IntoIterator<Item = String>) -> String {
            names
                .into_iter()
                .map(|name| encode_uri_component(&name).as_string().unwrap_or(name))
                .collect::<Vec<_>>()
                .join(",")
        }

        loop {
            let mut next_sleep_secs = GUILD_ONLINE_POLL_INTERVAL_SECS;
            if map_mode.get_untracked() == MapMode::Live {
                let map = territories.get_untracked();
                let mut guild_counts: HashMap<String, usize> = HashMap::new();
                for ct in map.values() {
                    *guild_counts
                        .entry(ct.territory.guild.name.clone())
                        .or_default() += 1;
                }
                let mut sorted: Vec<_> = guild_counts.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                sorted.truncate(20);
                let names: Vec<String> = sorted.into_iter().map(|(name, _)| name).collect();
                if !names.is_empty() {
                    let query = encoded_names_query(names);
                    let url = format!("/api/guilds/online?names={query}");
                    match gloo_net::http::Request::get(&url).send().await {
                        Ok(resp) if resp.ok() => {
                            if let Ok(raw) = resp.json::<HashMap<String, serde_json::Value>>().await
                            {
                                guild_online_data.set(parse_online_response(raw));
                            }
                        }
                        _ => {
                            next_sleep_secs = GUILD_ONLINE_ERROR_RETRY_SECS;
                        }
                    }
                } else {
                    // Territories may not be hydrated yet right after boot/reload.
                    next_sleep_secs = GUILD_ONLINE_BOOTSTRAP_RETRY_SECS;
                }
            }
            gloo_timers::future::sleep(std::time::Duration::from_secs(next_sleep_secs)).await;
        }
    });

    settings.persist(move || Layout {
        sidebar_open: sidebar_open.get(),
        sidebar_width: clamp_sidebar_width(sidebar_width.get()),
        war_panel_open: war_panel_open.get(),
        war_panel_width: clamp_war_panel_width(war_panel_width.get()),
        leaderboard_sort_by_sr: leaderboard_sort_by_sr.get(),
    });

    // Enable sidebar transitions only after initial mount to avoid first-paint animation flash.
    Effect::new(move || {
        sidebar_ready.set(true);
    });

    // Resize listener: update is_mobile when crossing breakpoint
    Effect::new({
        move || {
            use wasm_bindgen::prelude::*;
            let Some(window) = web_sys::window() else {
                return;
            };

            RESIZE_BINDING.with(|slot| {
                if let Some(old) = slot.borrow_mut().take() {
                    old.window
                        .remove_event_listener_with_callback(
                            "resize",
                            old._callback.as_ref().unchecked_ref(),
                        )
                        .ok();
                }
            });

            let cb = Closure::<dyn Fn()>::new(move || {
                let (w, _) = canvas_dimensions();
                let mobile = w < MOBILE_BREAKPOINT;
                if mobile != is_mobile.get_untracked() {
                    is_mobile.set(mobile);
                    // Clear peek when switching to desktop
                    if !mobile {
                        peek_territory.set(None);
                    }
                }
            });
            window
                .add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref())
                .ok();
            RESIZE_BINDING.with(|slot| {
                *slot.borrow_mut() = Some(ResizeBinding {
                    window: window.clone(),
                    _callback: cb,
                });
            });
        }
    });

    // Lazy-mount sidebar panel on first open to keep initial boot focused on live map rendering.
    Effect::new(move || {
        if sidebar_open.get() && !sidebar_loaded.get_untracked() {
            sidebar_loaded.set(true);
        }
    });

    // 1-second interval to advance the tick signal (triggers cooldown re-renders)
    Effect::new({
        move || {
            use wasm_bindgen::prelude::*;
            let Some(window) = web_sys::window() else {
                return;
            };

            TICK_INTERVAL_BINDING.with(|slot| {
                if let Some(old) = slot.borrow_mut().take() {
                    old.window.clear_interval_with_handle(old.interval_id);
                }
            });

            let cb = Closure::<dyn Fn()>::new(move || {
                tick.set(chrono::Utc::now().timestamp());
            });
            let Ok(interval_id) = window.set_interval_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                1_000,
            ) else {
                return;
            };
            TICK_INTERVAL_BINDING.with(|slot| {
                *slot.borrow_mut() = Some(TickIntervalBinding {
                    window: window.clone(),
                    interval_id,
                    _callback: cb,
                });
            });
        }
    });

    // Bootstrap live state over the compressed REST endpoint. EventSource snapshots are
    // intentionally uncompressed streams, so waiting for the initial SSE snapshot makes
    // cold loads sit behind a much larger transfer before the first map paint.
    Effect::new(move || {
        if live_bootstrap_started.get_untracked() {
            return;
        }
        live_bootstrap_started.set(true);

        wasm_bindgen_futures::spawn_local(async move {
            match live_feed::fetch_live_state().await {
                Ok(live_state) => {
                    if map_mode.get_untracked() != MapMode::Live {
                        return;
                    }
                    if let Some(current_seq) = last_live_seq.get_untracked()
                        && current_seq > live_state.seq
                    {
                        web_sys::console::info_1(
                            &format!(
                                "stale_live_bootstrap_ignored (current_seq={}, bootstrap_seq={})",
                                current_seq, live_state.seq
                            )
                            .into(),
                        );
                        return;
                    }
                    // The SSE snapshot landed first, and nothing since.
                    if feed.holds_snapshot(live_state.seq) {
                        return;
                    }
                    territories.set(from_snapshot(live_state.territories));
                    last_live_seq.set(Some(live_state.seq));
                    feed.snapshot_seq.set_value(Some(live_state.seq));
                }
                Err(error) => {
                    web_sys::console::warn_1(
                        &format!("live bootstrap fetch failed; waiting for SSE snapshot: {error}")
                            .into(),
                    );
                }
            }
        });
    });

    // Connect to SSE on mount
    Effect::new(move || {
        live_feed::connect(feed);
        on_cleanup(|| {
            live_feed::disconnect();
        });
    });

    // Once initial territory data arrives, let one frame commit and then remove the
    // static shell. The old fixed 800ms settle window made fast cached/API loads
    // feel slow even when the map was already ready to render.
    Effect::new(move || {
        let has_territories = !territories.get().is_empty();
        if !has_territories
            || deferred_boot_ready.get_untracked()
            || deferred_boot_timer_set.get_untracked()
        {
            return;
        }

        deferred_boot_timer_set.set(true);
        if let Some(window) = web_sys::window() {
            let cb = wasm_bindgen::closure::Closure::once(move || {
                deferred_boot_ready.set(true);
            });
            let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
            cb.forget();
        } else {
            deferred_boot_ready.set(true);
        }
    });

    // Keep shell step text tied to real startup milestones.
    Effect::new(move || {
        let has_territories = !territories.get().is_empty();
        if deferred_boot_ready.get() {
            set_loading_shell_step("Rendering map");
        } else if has_territories {
            set_loading_shell_step("Syncing territory data");
        } else {
            set_loading_shell_step("Connecting to API");
        }
    });

    // Remove static shell shortly after boot is ready so the final step is visible briefly.
    Effect::new(move || {
        if !deferred_boot_ready.get() || loading_shell_removed.get_untracked() {
            return;
        }
        loading_shell_removed.set(true);
        remove_loading_shell();
    });

    // Start background tile loading as soon as the map shell is ready. The tile
    // loader prioritizes visible tiles, so an extra idle wait delays first useful
    // background paint without reducing startup work.
    Effect::new(move || {
        if !deferred_boot_ready.get() || tile_fetch_scheduled.get_untracked() {
            return;
        }
        tile_fetch_scheduled.set(true);
        map.load_tiles();
    });

    // Start playback engine (runs continuously, only active when playing)
    Effect::new(move || {
        crate::playback::start_playback_engine();
    });

    // Global keyboard shortcuts
    let toggle_overlay = move |overlay: Overlay| {
        settings.edit(
            settings::OVERLAY,
            settings.get_untracked(settings::OVERLAY).toggled(overlay),
        )
    };
    Effect::new(move || {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::prelude::*;

        let Some(window) = web_sys::window() else {
            return;
        };

        KEYDOWN_BINDING.with(|slot| {
            if let Some(old) = slot.borrow_mut().take() {
                let _ = old.window.remove_event_listener_with_callback(
                    "keydown",
                    old._handler.as_ref().unchecked_ref(),
                );
            }
        });

        let handler =
            Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
                // The help dialog, the war sheet and the player detail handle their own keys,
                // and a key a control already acted on is not a map shortcut.
                if keybind_help_open.get_untracked()
                    || e.default_prevented()
                    || war_room.handle_stray_key(&e)
                {
                    return;
                }
                // Undo and redo a settings change while the menu is open. A text field keeps
                // its own undo.
                if show_settings.get_untracked()
                    && let Some(redo) = settings_undo_key(&e)
                    && !edits_text(e.target())
                {
                    e.prevent_default();
                    if redo {
                        settings.redo();
                    } else {
                        settings.undo();
                    }
                    return;
                }
                let key = e.key();
                let target_tag = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    .map(|el| el.tag_name())
                    .unwrap_or_default();
                let is_radio = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                    .is_some_and(|input| input.type_() == "radio");

                // Text and other input controls keep their existing key handling.
                if (target_tag == "INPUT" && !is_radio) || target_tag == "TEXTAREA" {
                    if key == "Escape"
                        && let Some(el) = e
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.blur().ok();
                    }
                    return;
                }
                // Radios keep native navigation/activation, but not map shortcuts.
                if is_radio
                    && matches!(
                        key.as_str(),
                        " " | "Enter" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
                    )
                {
                    return;
                }

                let Some(shortcut) = Shortcut::from_key(&key) else {
                    return;
                };
                // A focused control keeps its own Enter and Space activation.
                if matches!(shortcut, Shortcut::OpenListItem | Shortcut::PlayPause)
                    && matches!(target_tag.as_str(), "BUTTON" | "A" | "SELECT" | "SUMMARY")
                {
                    return;
                }
                match shortcut {
                    Shortcut::Dismiss => {
                        if selected.get_untracked().is_some() {
                            if let Some(return_guild) = detail_return_guild.get_untracked() {
                                selected.set(None);
                                selected_guild.set(Some(return_guild));
                            } else {
                                selected.set(None);
                                selected_guild.set(None);
                            }
                        } else {
                            selected.set(None);
                            selected_guild.set(None);
                        }
                        detail_return_guild.set(None);
                        peek_territory.set(None);
                        hovered.set(None);
                        if sidebar_transient.get_untracked() {
                            sidebar_open.set(false);
                            sidebar_transient.set(false);
                        }
                    }
                    Shortcut::FocusSearch => {
                        e.prevent_default();
                        let Some(window) = web_sys::window() else {
                            return;
                        };
                        let Some(doc) = window.document() else {
                            return;
                        };
                        if let Some(el) = doc.query_selector("[data-search-input]").ok().flatten()
                            && let Ok(input) = el.dyn_into::<web_sys::HtmlElement>()
                        {
                            input.focus().ok();
                        }
                    }
                    Shortcut::AbbreviateNames => settings.toggle(settings::ABBREVIATE_NAMES),
                    Shortcut::ShowNames => settings.toggle(settings::TERRITORY_NAMES),
                    Shortcut::Countdown => settings.toggle(settings::COOLDOWN_COUNTDOWN),
                    Shortcut::Connections => settings.toggle(settings::CONNECTIONS),
                    Shortcut::BoldConnections => settings.toggle(settings::BOLD_CONNECTIONS),
                    Shortcut::ReadableFont => settings.toggle(settings::READABLE_FONT),
                    Shortcut::ResourceHighlight => toggle_overlay(Overlay::Resources),
                    Shortcut::DefenseHighlight => toggle_overlay(Overlay::Defense),
                    Shortcut::MapIntel => toggle_overlay(Overlay::Intel),
                    Shortcut::Minimap => settings.toggle(settings::MINIMAP),
                    Shortcut::History => {
                        let mode = map_mode.get_untracked();
                        match mode {
                            MapMode::Live => {
                                if !history_available.get_untracked() {
                                    history::check_availability(history_available);
                                    return;
                                }
                                if history_available.get_untracked() {
                                    history::enter_history_mode(history::EnterHistoryModeInput {
                                        history_map_time,
                                        mode: map_mode,
                                        history_timestamp,
                                        history_bounds,
                                        history_fetch_nonce,
                                        history_legacy_geometry_active,
                                        history_buffered_updates,
                                        history_buffer_mode_active,
                                        needs_live_resync,
                                        history_scalar_sample: history_season_scalar_sample,
                                        history_sr_leaderboard: history_season_leaderboard,
                                        geo_store: territory_geometry,
                                        guild_color_store: guild_colors,
                                        territories,
                                    });
                                }
                            }
                            MapMode::History => {
                                history::exit_history_mode(history::ExitHistoryModeInput {
                                    mode: map_mode,
                                    playback_active,
                                    history_fetch_nonce,
                                    history_timestamp,
                                    history_legacy_geometry_active,
                                    history_buffered_updates,
                                    history_buffer_mode_active,
                                    last_live_seq,
                                    needs_live_resync,
                                    live_handoff_resync_count,
                                    history_sr_leaderboard: history_season_leaderboard,
                                    territories,
                                });
                            }
                        }
                    }
                    Shortcut::PlayPause => {
                        if map_mode.get_untracked() == MapMode::History {
                            e.prevent_default();
                            playback_active.update(|v| *v = !*v);
                        }
                    }
                    Shortcut::StepBack => {
                        if map_mode.get_untracked() == MapMode::History {
                            history::step_backward(history::HistoryStepContext {
                                history_timestamp,
                                playback_active,
                                fetch: history::HistoryFetchContext {
                                    history_map_time,
                                    mode: map_mode,
                                    history_fetch_nonce,
                                    history_legacy_geometry_active,
                                    history_scalar_sample: history_season_scalar_sample,
                                    history_sr_leaderboard: history_season_leaderboard,
                                    geo_store: territory_geometry,
                                    guild_color_store: guild_colors,
                                    territories,
                                },
                            });
                        }
                    }
                    Shortcut::StepForward => {
                        if map_mode.get_untracked() == MapMode::History {
                            history::step_forward(history::HistoryStepContext {
                                history_timestamp,
                                playback_active,
                                fetch: history::HistoryFetchContext {
                                    history_map_time,
                                    mode: map_mode,
                                    history_fetch_nonce,
                                    history_legacy_geometry_active,
                                    history_scalar_sample: history_season_scalar_sample,
                                    history_sr_leaderboard: history_season_leaderboard,
                                    geo_store: territory_geometry,
                                    guild_color_store: guild_colors,
                                    territories,
                                },
                            });
                        }
                    }
                    Shortcut::FitMap => {
                        let bounds = territories.with_untracked(|territories| {
                            territories.values().fold(None, |bounds, ct| {
                                let loc = &ct.territory.location;
                                let (left, top) = (loc.left() as f64, loc.top() as f64);
                                let (right, bottom) = (loc.right() as f64, loc.bottom() as f64);
                                Some(match bounds {
                                    None => (left, top, right, bottom),
                                    Some((min_x, min_y, max_x, max_y)) => (
                                        f64::min(min_x, left),
                                        f64::min(min_y, top),
                                        f64::max(max_x, right),
                                        f64::max(max_y, bottom),
                                    ),
                                })
                            })
                        });
                        if let Some(bounds) = bounds {
                            camera.fit(bounds);
                        }
                    }
                    Shortcut::ListNext => {
                        e.prevent_default();
                        let items = sidebar_items.get_untracked();
                        if !items.is_empty() {
                            sidebar_index.update(|i| *i = (*i + 1).min(items.len() - 1));
                        }
                    }
                    Shortcut::ListPrevious => {
                        e.prevent_default();
                        let items = sidebar_items.get_untracked();
                        if !items.is_empty() {
                            sidebar_index.update(|i| *i = i.saturating_sub(1));
                        }
                    }
                    Shortcut::OpenListItem => {
                        let items = sidebar_items.get_untracked();
                        let idx = sidebar_index.get_untracked();
                        if let Some(name) = items.get(idx) {
                            let map = territories.get_untracked();
                            let has_active_search = !search_query.get_untracked().trim().is_empty();
                            if has_active_search {
                                // Search results are territory names.
                                detail_return_guild.set(None);
                                selected.set(Some(name.clone()));
                                if let Some(ct) = map.get(name) {
                                    camera.focus(&ct.territory.location);
                                }
                            } else {
                                // Guild name (from leaderboard) — open guild panel
                                selected_guild.set(Some(name.clone()));
                            }
                            if map_mode.get_untracked() == MapMode::Live
                                && !sidebar_open.get_untracked()
                            {
                                sidebar_open.set(true);
                            }
                        }
                    }
                    Shortcut::PanLeft => {
                        e.prevent_default();
                        camera.update(|vp| vp.pan(50.0 / vp.scale, 0.0));
                    }
                    Shortcut::PanRight => {
                        e.prevent_default();
                        camera.update(|vp| vp.pan(-50.0 / vp.scale, 0.0));
                    }
                    Shortcut::ZoomIn => {
                        e.prevent_default();
                        camera.zoom_at_center(-120.0);
                    }
                    Shortcut::ZoomOut => {
                        e.prevent_default();
                        camera.zoom_at_center(120.0);
                    }
                }
            });

        if window
            .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
            .is_ok()
        {
            KEYDOWN_BINDING.with(|slot| {
                *slot.borrow_mut() = Some(KeydownBinding {
                    window: window.clone(),
                    _handler: handler,
                });
            });
        }
    });

    // Suppress transition flash when crossing the mobile breakpoint.
    // Temporarily disable sidebar_ready (which controls transition CSS) and re-enable after 50ms.
    {
        let prev_mobile: std::cell::Cell<Option<bool>> = std::cell::Cell::new(None);
        Effect::new(move || {
            let mobile = is_mobile.get();
            let was = prev_mobile.get();
            prev_mobile.set(Some(mobile));
            if was.is_some() && was != Some(mobile) {
                sidebar_ready.set(false);
                if let Some(window) = web_sys::window() {
                    let cb = wasm_bindgen::closure::Closure::once(move || {
                        sidebar_ready.set(true);
                    });
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        50,
                    );
                    cb.forget();
                }
            }
        });
    }

    let on_map_event = move |event: MapEvent| match event {
        // On mobile a tap only peeks; the card's Details action opens the full panel.
        // Hover leaves the card alone so a narrow mouse window can still reach Details.
        MapEvent::Tap { territory, .. } if is_mobile.get_untracked() => {
            selected.set(None);
            detail_return_guild.set(None);
            peek_territory.set(territory);
        }
        MapEvent::Tap { territory, .. } => {
            if territory.is_some() {
                show_settings.set(false);
                if !sidebar_open.get_untracked() {
                    sidebar_open.set(true);
                    sidebar_transient.set(true);
                }
            } else if sidebar_transient.get_untracked() {
                sidebar_open.set(false);
                sidebar_transient.set(false);
            }
            detail_return_guild.set(None);
            selected.set(territory);
        }
        // The live map has no edit tools.
        MapEvent::Hover(_)
        | MapEvent::Stroke(_)
        | MapEvent::Pick { .. }
        | MapEvent::BoxSelect { .. }
        | MapEvent::SelectTap { .. } => {}
    };

    let sheet_drag: RwSignal<Option<SheetDrag>> = RwSignal::new(None);

    view! {
        <div style="width: 100%; height: 100%; position: relative;">
            <div style="width: 100%; height: 100%; position: relative; overflow: hidden; background: var(--bg-page);">
                <MapCanvas map on_event=on_map_event />
                <HeatLegend />
                <SiteNavbar />
                // Minimap backdrop frame (desktop only)
                <div
                    style:display=move || if is_mobile.get() || !show_minimap.get() { "none" } else { "block" }
                    style:bottom=move || format!("{}px", minimap_inset.get().unwrap_or(MINIMAP_LIVE_INSET))
                    style="position: absolute; left: 16px; z-index: 6; width: 200px; height: 280px; pointer-events: none; border: 1px solid rgba(58,63,92,0.6); border-radius: 4px; box-shadow: 0 4px 20px rgba(0,0,0,0.5), 0 0 1px rgba(168,85,247,0.12), inset 0 0 0 1px rgba(255,255,255,0.03);"
                >
                    // "MAP" label
                    <div style="position: absolute; top: 6px; left: 8px; font-family: var(--font-display); font-size: 0.62rem; color: rgba(245,197,66,0.5); letter-spacing: 0.1em;">"MAP"</div>
                    // Gold corner marks — top-left
                    <div style="position: absolute; top: 0; left: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3);" />
                    <div style="position: absolute; top: 0; left: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3);" />
                    // Gold corner marks — top-right
                    <div style="position: absolute; top: 0; right: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3);" />
                    <div style="position: absolute; top: 0; right: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3);" />
                    // Gold corner marks — bottom-left
                    <div style="position: absolute; bottom: 0; left: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3);" />
                    <div style="position: absolute; bottom: 0; left: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3);" />
                    // Gold corner marks — bottom-right
                    <div style="position: absolute; bottom: 0; right: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3);" />
                    <div style="position: absolute; bottom: 0; right: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3);" />
                </div>
                <DefenseLegend />
                <MapIntelOverlay />
                <crate::players::PlayerHeadsOverlay />
                <crate::war_room::WarRoomHud />
                // Mobile HUD buttons — bottom-right stack
                <MobileHistoryToggle />
                // Mobile FAB — opens sidebar
                <button
                    class="mobile-sidebar-fab"
                    style:display=move || {
                        if is_mobile.get() && !sidebar_open.get() { "flex" } else { "none" }
                    }
                    style="position: absolute; bottom: 16px; right: 16px; z-index: 20; width: 48px; height: 48px; border-radius: 12px; background: var(--color-deep); border: 1px solid var(--color-border-accent); align-items: center; justify-content: center; cursor: pointer; box-shadow: 0 4px 16px rgba(0,0,0,0.5), 0 0 1px rgba(168,85,247,0.15); color: var(--color-gold); font-size: 1.4rem; font-family: var(--font-mono); touch-action: manipulation;"
                    aria-label="Open sidebar"
                    on:click=move |_| {
                        sidebar_open.set(true);
                        sidebar_transient.set(false);
                    }
                >
                    "\u{2630}"
                </button>
            </div>
            // Scrim overlay for mobile bottom sheet
            <div
                class="bottom-sheet-scrim"
                class:scrim-visible=move || is_mobile.get() && sidebar_open.get()
                on:click=move |_| {
                    if is_mobile.get() {
                        sidebar_open.set(false);
                    }
                }
            />
            <div
                class="sidebar-wrapper"
                class:sidebar-ready=move || sidebar_ready.get()
                class:sidebar-open=move || sidebar_open.get()
                class:sheet-dragging=move || sheet_drag.with(Option::is_some)
                style:width=move || {
                    if is_mobile.get() {
                        "100%".to_string()
                    } else {
                        format!("{:.0}px", sidebar_width.get())
                    }
                }
                style:transform=move || {
                    sidebar_transform(
                        is_mobile.get(),
                        sidebar_open.get(),
                        sheet_drag.with(|drag| drag.map(|drag| drag.offset)),
                    )
                }
                style:pointer-events=move || if sidebar_open.get() { "auto" } else { "none" }
            >
                <BottomSheetHandle drag=sheet_drag />
                <SidebarResizeHandle />
                <SidebarToggle />
                {move || {
                    if sidebar_loaded.get() {
                        view! { <Sidebar /> }.into_any()
                    } else {
                        ().into_any()
                    }
                }}
            </div>
            {move || {
                if map_mode.get() == MapMode::History {
                    view! { <Timeline /> }.into_any()
                } else {
                    ().into_any()
                }
            }}
        </div>
        // Tooltip only on desktop; TerritoryPeekCard handles mobile feedback
        {move || {
            if !is_mobile.get() {
                view! { <Tooltip /> }.into_any()
            } else {
                ().into_any()
            }
        }}
        <TerritoryPeekCard />
    }
}

/// Colour scale for heat mode, over the top-left of the map.
///
/// Heat colours rank territories by their counts rather than scale the counts (see
/// [`HeatScale`]), so the bar is labelled with the counts at each quarter of the ranked
/// territories, which are not evenly spaced counts, each under the colour it renders.
#[component]
fn HeatLegend() -> impl IntoView {
    let heat_mode_enabled = expect_context::<ViewerSettings>().value(settings::HEAT_MAP);
    let HeatScaleSignal(heat_scale) = expect_context();
    let HeatWindowLabel(heat_window_label) = expect_context();

    move || {
        if !heat_mode_enabled.get() {
            return ().into_any();
        }
        let scale = heat_scale.get();
        let stops = heat_legend_stops(&scale);
        let rows = heat_legend_rows(&stops);
        let row_count = rows.iter().max().map_or(1, |row| row + 1);
        let uniform = stops.len() == 1;
        let labels = stops
            .into_iter()
            .zip(rows)
            .map(|((share, count), row)| {
                // The label's own point at the same share sits on its colour, so labels at the
                // ends stay inside the bar and one in the middle centres.
                let percent = share * 100.0;
                let top = row as f64 * HEAT_LEGEND_ROW_REM;
                let style = format!(
                    "position: absolute; left: {percent}%; top: {top}rem; transform: translateX(-{percent}%); white-space: nowrap;"
                );
                view! { <span style=style>{count}</span> }
            })
            .collect_view();
        let label_rows_height = format!("{}rem", row_count as f64 * HEAT_LEGEND_ROW_REM);
        let caption = match scale.len() {
            0 => "No captures in this period".to_string(),
            1 => "1 territory taken".to_string(),
            ranked if uniform => format!("All {ranked} territories taken alike"),
            ranked => format!("Quartiles of {ranked} territories taken"),
        };
        let label = heat_window_label.get();
        view! {
            <div style="position: absolute; top: calc(var(--nav-height, 0px) + 16px); left: 16px; z-index: 22; pointer-events: none; background: rgba(10,12,20,0.82); border: 1px solid rgba(245,197,66,0.25); border-radius: 6px; padding: 8px 10px; min-width: 172px;">
                <div style="font-family: var(--font-display); font-size: 0.62rem; letter-spacing: 0.08em; text-transform: uppercase; color: var(--color-gold); margin-bottom: 5px;">"Heat"</div>
                <div style="height: 8px; border-radius: 0; background: linear-gradient(90deg, #1e50dc 0%, #28c8f0 25%, #f5dc46 50%, #f58c32 75%, #dc2823 100%);" />
                <div
                    style="position: relative; margin-top: 5px; font-family: var(--font-mono); font-size: 0.62rem; color: var(--color-text-secondary);"
                    style:height=label_rows_height
                    // Label boxes are a line high, a little less than a row, so stacked labels
                    // never touch.
                    style:line-height=format!("{HEAT_LEGEND_LINE_REM}rem")
                >
                    {labels}
                </div>
                <div style="margin-top: 3px; font-family: var(--font-mono); font-size: 0.6rem; color: var(--color-text-secondary);">
                    {caption}
                </div>
                <div style="margin-top: 4px; font-family: var(--font-mono); font-size: 0.6rem; color: #6f748f; line-height: 1.25;">
                    {label}
                </div>
            </div>
        }
        .into_any()
    }
}

/// The legend's labels as (share of the bar, count): the counts at 0, 25, 50, 75 and 100% of
/// the ranked territories, each once and at the share the map colours it with. Tied counts
/// can fill several quarters, and all of them render at the share of their last territory, so
/// a uniform period is one label at the red end. None without captures.
fn heat_legend_stops(scale: &HeatScale) -> Vec<(f64, String)> {
    let mut counts: Vec<u64> = [0.0, 0.25, 0.5, 0.75, 1.0]
        .into_iter()
        .filter_map(|share| scale.count_at(share))
        .collect();
    // Ascending, so repeats are neighbours.
    counts.dedup();
    counts
        .into_iter()
        .map(|count| (scale.intensity(count), format_heat_count(count)))
        .collect()
}

/// Height of one row of legend labels.
const HEAT_LEGEND_ROW_REM: f64 = 0.9;
/// Line height of a legend label: its box, which must fit inside a row.
const HEAT_LEGEND_LINE_REM: f64 = 0.8;
const _: () = assert!(HEAT_LEGEND_LINE_REM < HEAT_LEGEND_ROW_REM);
/// The legend's bar in label characters, at its narrowest: 172 px less 20 px of padding, at
/// about 5.2 px per character of the monospace labels (0.6 of 0.62rem, at the map's 14 px
/// root size).
const HEAT_LEGEND_BAR_CHARS: f64 = 29.0;

/// A row for each of the legend's labels, in bar order. Labels keep their positions; each goes
/// on the first row where it clears the last label by a character, and on a new row below
/// when none has room, as when nearly every territory shares one count and the next is at the
/// red end. A label at share `s` is shifted back by `s` of its own width, so in characters it
/// spans `s * (bar - len)` to that plus `len`.
fn heat_legend_rows(stops: &[(f64, String)]) -> Vec<usize> {
    let mut row_ends: Vec<f64> = Vec::new();
    stops
        .iter()
        .map(|(share, label)| {
            let len = label.chars().count() as f64;
            let start = share * (HEAT_LEGEND_BAR_CHARS - len);
            let row = row_ends
                .iter()
                .position(|&end| start - end >= 1.0)
                .unwrap_or_else(|| {
                    row_ends.push(0.0);
                    row_ends.len() - 1
                });
            row_ends[row] = start + len;
            row
        })
        .collect()
}

/// Exact below ten thousand, then thousands to one decimal ("44.2k").
fn format_heat_count(count: u64) -> String {
    if count < 10_000 {
        count.to_string()
    } else {
        format!("{:.1}k", count as f64 / 1000.0)
    }
}

/// Toggle button for showing/hiding the sidebar. Attached to the sidebar's left edge.
#[component]
fn SidebarToggle() -> impl IntoView {
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();

    view! {
        <button
            type="button"
            class="sidebar-toggle"
            title=move || if sidebar_open.get() { "Hide sidebar" } else { "Show sidebar" }
            aria-label=move || if sidebar_open.get() { "Hide sidebar" } else { "Show sidebar" }
            on:click=move |_| {
                sidebar_open.update(|v| *v = !*v);
                sidebar_transient.set(false);
            }
        >
            {move || if sidebar_open.get() { "\u{00BB}" } else { "\u{00AB}" }}
        </button>
    }
}

#[component]
fn SidebarResizeHandle() -> impl IntoView {
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();
    let IsMobile(is_mobile) = expect_context();

    let drag_start_x = Rc::new(std::cell::Cell::new(0.0f64));
    let drag_start_width = Rc::new(std::cell::Cell::new(0.0f64));
    let dragging: RwSignal<bool> = RwSignal::new(false);
    let active_pointer_id = Rc::new(std::cell::Cell::new(None::<i32>));

    let drag_start_x_down = drag_start_x.clone();
    let drag_start_width_down = drag_start_width.clone();
    let active_pointer_id_down = active_pointer_id.clone();
    let drag_start_x_move = drag_start_x.clone();
    let drag_start_width_move = drag_start_width.clone();
    let active_pointer_id_move = active_pointer_id.clone();
    let active_pointer_id_end = active_pointer_id.clone();

    let end_drag: Rc<dyn Fn(web_sys::PointerEvent)> = Rc::new(move |e: web_sys::PointerEvent| {
        if active_pointer_id_end.get() != Some(e.pointer_id()) {
            return;
        }
        dragging.set(false);
        active_pointer_id_end.set(None);
        if let Some(target) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
        {
            target.release_pointer_capture(e.pointer_id()).ok();
        }
    });

    let end_drag_up = end_drag.clone();
    let end_drag_cancel = end_drag.clone();

    view! {
        <div
            class="sidebar-resize-handle"
            class:sidebar-resize-active=move || dragging.get()
            style:display=move || {
                if !is_mobile.get() && sidebar_open.get() {
                    "block"
                } else {
                    "none"
                }
            }
            on:pointerdown=move |e: web_sys::PointerEvent| {
                if !e.is_primary() || e.button() != 0 || is_mobile.get_untracked() || !sidebar_open.get_untracked() {
                    return;
                }
                e.prevent_default();
                dragging.set(true);
                active_pointer_id_down.set(Some(e.pointer_id()));
                drag_start_x_down.set(e.client_x() as f64);
                drag_start_width_down.set(sidebar_width.get_untracked());
                if let Some(target) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    target.set_pointer_capture(e.pointer_id()).ok();
                }
            }
            on:pointermove=move |e: web_sys::PointerEvent| {
                if !dragging.get_untracked() || active_pointer_id_move.get() != Some(e.pointer_id()) {
                    return;
                }
                e.prevent_default();
                let next_width = clamp_sidebar_width(
                    drag_start_width_move.get() + (drag_start_x_move.get() - e.client_x() as f64),
                );
                sidebar_width.set(next_width);
            }
            on:pointerup=move |e: web_sys::PointerEvent| {
                end_drag_up(e);
            }
            on:pointercancel=move |e: web_sys::PointerEvent| {
                end_drag_cancel(e);
            }
        />
    }
}

/// How far down the mobile sheet must be dragged to close it, CSS pixels.
const SHEET_DISMISS_DRAG_PX: f64 = 80.0;

/// A drag of the mobile bottom sheet's handle by one pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SheetDrag {
    pointer_id: i32,
    start_y: f64,
    /// How far the sheet follows the pointer down, CSS pixels.
    offset: f64,
}

impl SheetDrag {
    fn new(pointer_id: i32, y: f64) -> Self {
        Self {
            pointer_id,
            start_y: y,
            offset: 0.0,
        }
    }

    /// The drag after its pointer moved to `y`; the sheet never rises above its open place.
    fn moved_to(self, y: f64) -> Self {
        Self {
            offset: (y - self.start_y).max(0.0),
            ..self
        }
    }

    fn dismisses(&self) -> bool {
        self.offset > SHEET_DISMISS_DRAG_PX
    }
}

/// Where the sidebar sits: a bottom sheet on mobile, held down by a handle drag while one
/// lasts, else a panel at the right edge.
fn sidebar_transform(mobile: bool, open: bool, drag_offset: Option<f64>) -> String {
    match (mobile, open, drag_offset) {
        (true, true, Some(offset)) => format!("translateY({offset}px)"),
        (true, true, None) => "translateY(0)".to_string(),
        (true, false, _) => "translateY(100%)".to_string(),
        (false, true, _) => "translateX(0)".to_string(),
        (false, false, _) => "translateX(100%)".to_string(),
    }
}

/// Swipe-to-dismiss drag handle for the mobile bottom sheet. It only updates `drag`; the
/// sheet's transform follows from that and the open state, so however a drag ends, the
/// sheet is left where the open state puts it.
#[component]
fn BottomSheetHandle(drag: RwSignal<Option<SheetDrag>>) -> impl IntoView {
    let SidebarOpen(sidebar_open) = expect_context();
    let IsMobile(is_mobile) = expect_context();

    // Closing the sheet, or leaving the mobile layout, ends a drag in progress.
    Effect::new(move || {
        if (!sidebar_open.get() || !is_mobile.get()) && drag.with_untracked(Option::is_some) {
            drag.set(None);
        }
    });

    let owns = move |e: &web_sys::PointerEvent| {
        drag.with_untracked(|drag| drag.is_some_and(|drag| drag.pointer_id == e.pointer_id()))
    };
    let end = move |e: web_sys::PointerEvent| {
        if owns(&e) {
            drag.set(None);
        }
    };

    view! {
        <div
            class="bottom-sheet-handle"
            on:pointerdown=move |e: web_sys::PointerEvent| {
                if !is_mobile.get_untracked() || !sidebar_open.get_untracked() {
                    return;
                }
                let Some(handle) = e
                    .current_target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                else {
                    return;
                };
                // One pointer drags at a time; a drag whose pointer the handle no longer
                // holds is over even if no event said so.
                if drag.with_untracked(|drag| {
                    drag.is_some_and(|drag| handle.has_pointer_capture(drag.pointer_id))
                }) {
                    return;
                }
                drag.set(Some(SheetDrag::new(e.pointer_id(), f64::from(e.client_y()))));
                handle.set_pointer_capture(e.pointer_id()).ok();
            }
            on:pointermove=move |e: web_sys::PointerEvent| {
                if owns(&e) {
                    let y = f64::from(e.client_y());
                    drag.update(|drag| *drag = drag.map(|drag| drag.moved_to(y)));
                }
            }
            on:pointerup=move |e: web_sys::PointerEvent| {
                if !owns(&e) {
                    return;
                }
                let dismisses = drag
                    .get_untracked()
                    .is_some_and(|drag| drag.moved_to(f64::from(e.client_y())).dismisses());
                drag.set(None);
                if dismisses {
                    sidebar_open.set(false);
                }
            }
            on:pointercancel=end
            on:lostpointercapture=end
        />
    }
}

/// Mobile-only floating button to toggle history mode without opening the sidebar.
#[component]
fn MobileHistoryToggle() -> impl IntoView {
    let IsMobile(is_mobile) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let HistoryAvailable(history_available) = expect_context();
    let HistoryLegacyGeometryActive(history_legacy_geometry_active) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let HistoryMapTime(history_map_time) = expect_context();
    let HistoryBoundsSignal(history_bounds) = expect_context();
    let HistoryFetchNonce(history_fetch_nonce) = expect_context();
    let PlaybackActive(playback_active) = expect_context();
    let LastLiveSeq(last_live_seq) = expect_context();
    let HistoryBufferedUpdates(history_buffered_updates) = expect_context();
    let HistoryBufferModeActive(history_buffer_mode_active) = expect_context();
    let HistorySeasonScalarSample(history_season_scalar_sample) = expect_context();
    let HistorySeasonLeaderboard(history_season_leaderboard) = expect_context();
    let NeedsLiveResync(needs_live_resync) = expect_context();
    let LiveHandoffResyncCount(live_handoff_resync_count) = expect_context();
    let TerritoryGeometryStore(geo_store) = expect_context();
    let GuildColorStore(guild_color_store) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();

    let is_history = move || map_mode.get() == MapMode::History;

    view! {
        <button
            class="mobile-history-toggle"
            aria-label="History"
            style:display=move || {
                if is_mobile.get() && !sidebar_open.get() && history_available.get() && !is_history() {
                    "flex"
                } else {
                    "none"
                }
            }
            style:bottom=move || if is_history() { "76px" } else { "72px" }
            style:background=move || if is_history() { "var(--color-gold)" } else { "var(--color-deep)" }
            style:color=move || if is_history() { "var(--color-deep)" } else { "var(--color-text-secondary)" }
            style:border-color=move || if is_history() { "var(--color-gold)" } else { "var(--color-border-accent)" }
            style:box-shadow=move || {
                if is_history() {
                    "0 0 12px rgba(245,197,66,0.4), 0 4px 16px rgba(0,0,0,0.5)"
                } else {
                    "0 4px 16px rgba(0,0,0,0.5), 0 0 1px rgba(168,85,247,0.15)"
                }
            }
            style="position: absolute; right: 16px; z-index: 20; width: 48px; height: 48px; border-radius: 12px; border: 1px solid; align-items: center; justify-content: center; cursor: pointer; touch-action: manipulation; transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease;"
            on:click=move |_| {
                if is_history() {
                    history::exit_history_mode(history::ExitHistoryModeInput {
                        mode: map_mode,
                        playback_active,
                        history_fetch_nonce,
                        history_timestamp,
                        history_legacy_geometry_active,
                        history_buffered_updates,
                        history_buffer_mode_active,
                        last_live_seq,
                        needs_live_resync,
                        live_handoff_resync_count,
                        history_sr_leaderboard: history_season_leaderboard,
                        territories,
                    });
                } else {
                    history::enter_history_mode(history::EnterHistoryModeInput {
                        history_map_time,
                        mode: map_mode,
                        history_timestamp,
                        history_bounds,
                        history_fetch_nonce,
                        history_legacy_geometry_active,
                        history_buffered_updates,
                        history_buffer_mode_active,
                        needs_live_resync,
                        history_scalar_sample: history_season_scalar_sample,
                        history_sr_leaderboard: history_season_leaderboard,
                        geo_store,
                        guild_color_store,
                        territories,
                    });
                }
            }
        >
            <svg
                width="18" height="18" viewBox="0 0 16 16" fill="currentColor"
                xmlns="http://www.w3.org/2000/svg"
            >
                <path d="M8 0a8 8 0 1 0 0 16A8 8 0 0 0 8 0Zm0 14.4A6.4 6.4 0 1 1 8 1.6a6.4 6.4 0 0 1 0 12.8ZM8.4 4H7.2v4.8l4.2 2.52.6-1-3.6-2.12V4Z"/>
            </svg>
        </button>
    }
}

#[derive(Clone, PartialEq)]
struct TooltipInfo {
    name: String,
    guild_name: String,
    guild_prefix: String,
    held: String,
    guild_color: (u8, u8, u8),
    cooldown: Option<(String, f64)>,
    treasury: TreasuryLevel,
    resources: Vec<ResourceRow>,
    defense_tier: Option<String>,
    is_headquarters: bool,
    takes_in_window: Option<u64>,
}

#[component]
fn DefenseLegend() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let defense_highlight = Memo::new(move |_| settings.with(|s| s.overlay == Overlay::Defense));
    let IsMobile(is_mobile) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();

    view! {
        <div
            style:display=move || if defense_highlight.get() { "block" } else { "none" }
            style:right=move || {
                if !is_mobile.get() {
                    if sidebar_open.get() {
                        format!("{:.0}px", sidebar_width.get() + 16.0)
                    } else {
                        "64px".to_string()
                    }
                } else {
                    "16px".to_string()
                }
            }
            style="position: absolute; top: calc(var(--nav-height) + 16px); z-index: 8; pointer-events: none; padding: 8px 9px; border: 1px solid rgba(58,63,92,0.78); border-radius: 4px; background: rgba(19,22,31,0.92); box-shadow: 0 8px 24px rgba(0,0,0,0.34);"
        >
            <div style="font-family: var(--font-display); font-size: 0.64rem; letter-spacing: 0.12em; text-transform: uppercase; color: var(--color-text-secondary); margin-bottom: 6px;">
                "Defense"
            </div>
            <div style="display: grid; grid-template-columns: auto auto; gap: 4px 8px; align-items: center;">
                {sequoia_shared::tower::DefenseRating::ALL.iter().map(|rating| view! {
                    <span style={format!("width: 10px; height: 10px; border-radius: 2px; background: {}; border: 1px solid rgba(255,255,255,0.18);", rating.color_hex())} />
                    <span style="font-family: var(--font-mono); font-size: 0.64rem; color: #d8d5cb; white-space: nowrap;">
                        {rating.label()}
                    </span>
                }).collect_view()}
            </div>
        </div>
    }
}

/// Tooltip that follows the mouse cursor when hovering a territory.
#[component]
fn Tooltip() -> impl IntoView {
    let Hovered(hovered) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let pointer = expect_context::<BrowserMap>().pointer();
    let tick: RwSignal<i64> = expect_context();
    let CurrentMode(mode) = expect_context();
    let HistoryMapTime(history_map_time) = expect_context();
    let heat_mode_enabled = expect_context::<ViewerSettings>().value(settings::HEAT_MAP);
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();

    let tooltip_info = Memo::new(move |_| {
        let reference_secs = if mode.get() == MapMode::History {
            history_map_time.get().unwrap_or_else(|| tick.get())
        } else {
            tick.get()
        };
        let name = hovered.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let runtime = ct.territory.runtime.as_ref();
        let resources = territory_resource_rows(&ct.territory.resources, runtime);
        let defense_tier = runtime.and_then(|runtime| runtime.defense_tier.clone());
        let is_headquarters = runtime
            .and_then(|runtime| runtime.headquarters)
            .unwrap_or(false);
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
        let secs = (reference_secs - ct.territory.acquired.timestamp()).max(0);
        let treasury = treasury(runtime, secs);
        let cooldown = cooldown(secs);

        Some(TooltipInfo {
            name,
            guild_name: ct.territory.guild.name.clone(),
            guild_prefix: ct.territory.guild.prefix.clone(),
            held: format_hms(secs),
            guild_color: ct.guild_color,
            cooldown,
            treasury,
            resources,
            defense_tier,
            is_headquarters,
            takes_in_window,
        })
    });

    // Keyed by territory so the war line keeps its own state while the pointer stays on one.
    let hovered_name =
        Memo::new(move |_| tooltip_info.with(|info| info.as_ref().map(|info| info.name.clone())));

    view! {
        {move || {
            let Some(info) = tooltip_info.get() else {
                return ().into_any();
            };
            let (x, y) = pointer.get();
            let (r, g, b) = info.guild_color;
            let territory = Signal::derive(move || hovered_name.get().unwrap_or_default());
            view! {
                <div
                    class="tooltip-animate tcard tcard-hover"
                    style:left=format!("{}px", x + 16.0)
                    style:top=format!("{}px", y - 8.0)
                    style:--tcard-guild=rgba_css(r, g, b, 0.9)
                >
                    <TerritoryHead name=info.name.clone() hq=info.is_headquarters />
                    <div class="tcard-owner">
                        <a
                            class="tcard-guild"
                            href=crate::guild_stats_url(&info.guild_name)
                            target="_blank"
                            rel="noopener noreferrer"
                            title="Open Wynncraft guild stats"
                        >
                            {info.guild_name.clone()}
                        </a>
                        <span class="tcard-tag">"[" {info.guild_prefix.clone()} "]"</span>
                        <span class="tcard-held" title="Held for">{info.held.clone()}</span>
                    </div>
                    <TerritoryWar territory />
                    <dl class="tcard-stats">
                        <div>
                            <dt>"Cooldown"</dt>
                            {match info.cooldown.clone() {
                                Some((remaining, frac)) => view! {
                                    <dd class="tcard-cooldown">
                                        {remaining}
                                        <span class="tcard-meter" aria-hidden="true">
                                            <span style:width=format!("{:.1}%", frac * 100.0) />
                                        </span>
                                    </dd>
                                }.into_any(),
                                None => view! { <dd class="tcard-ready">"Ready"</dd> }.into_any(),
                            }}
                        </div>
                        <div><dt>"Treasury"</dt><TreasuryValue level=info.treasury buff=true /></div>
                        {info.defense_tier.clone().map(|tier| view! {
                            <div><dt>"Defense"</dt><DefenseValue tier /></div>
                        })}
                        {info.takes_in_window.map(|count| view! {
                            <div><dt>"Takes"</dt><dd>{count}</dd></div>
                        })}
                    </dl>
                    <ResourceTable rows=info.resources.clone() />
                </div>
            }.into_any()
        }}
    }
}

/// Mobile peek card shown on territory tap. Displays summary info with a button to open full details.
#[component]
fn TerritoryPeekCard() -> impl IntoView {
    let PeekTerritory(peek_territory) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let tick: RwSignal<i64> = expect_context();
    let CurrentMode(mode) = expect_context();
    let HistoryMapTime(history_map_time) = expect_context();
    let HistoryBarHeight(history_bar_height) = expect_context();
    let Selected(selected) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let heat_mode_enabled = expect_context::<ViewerSettings>().value(settings::HEAT_MAP);
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();

    let peek_info = Memo::new(move |_| {
        let reference_secs = if mode.get() == MapMode::History {
            history_map_time.get().unwrap_or_else(|| tick.get())
        } else {
            tick.get()
        };
        let name = peek_territory.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let (r, g, b) = ct.guild_color;
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
        let secs = (reference_secs - ct.territory.acquired.timestamp()).max(0);
        let held = format_hms(secs);
        let treasury = treasury(ct.territory.runtime.as_ref(), secs);
        Some((
            name,
            ct.territory.guild.name.clone(),
            ct.territory.guild.prefix.clone(),
            held,
            (r, g, b),
            treasury,
            takes_in_window,
        ))
    });

    view! {
        {move || {
            // The open sheet covers the card; it comes back when the sheet closes. The card
            // stops short of the right-hand button stack so the menu stays reachable.
            let Some(info) = peek_info.get().filter(|_| !sidebar_open.get()) else {
                return view! { <div style="display:none;" /> }.into_any();
            };
            let (r, g, b) = info.4;
            let treasury = info.5;
            let takes_in_window = info.6;
            let name = info.0.clone();
            // In History the card stands clear of the bar, whose height already includes the
            // safe area.
            let bottom = move || {
                if mode.get() == MapMode::History {
                    format!("{:.0}px", history_bar_height.get() + PEEK_GAP_PX)
                } else {
                    format!("calc({PEEK_GAP_PX}px + env(safe-area-inset-bottom, 0px))")
                }
            };
            let territory = Signal::stored(name.clone());
            view! {
                <div
                    class="peek-card-animate tcard tcard-peek"
                    style:bottom=bottom
                    style:--tcard-guild=rgba_css(r, g, b, 0.9)
                >
                    <div class="tcard-peek-body">
                        <TerritoryHead name=info.0.clone() hq=false />
                        <div class="tcard-owner">
                            <span class="tcard-guild">{info.1}</span>
                            <span class="tcard-tag">"[" {info.2.clone()} "]"</span>
                            <span class="tcard-held" title="Held for">{info.3}</span>
                        </div>
                        <TerritoryWar territory />
                        <dl class="tcard-stats">
                            <div><dt>"Treasury"</dt><TreasuryValue level=treasury /></div>
                            {takes_in_window.map(|count| view! {
                                <div><dt>"Takes"</dt><dd>{count}</dd></div>
                            })}
                        </dl>
                    </div>
                    <button
                        type="button"
                        class="tcard-peek-details"
                        on:click=move |_| {
                            show_settings.set(false);
                            detail_return_guild.set(None);
                            selected.set(Some(name.clone()));
                            sidebar_open.set(true);
                            peek_territory.set(None);
                        }
                    >
                        "Details"
                    </button>
                </div>
            }.into_any()
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HEAT_LEGEND_BAR_CHARS, HeatScale, format_heat_count, heat_legend_rows, heat_legend_stops,
    };
    use super::{
        MapMode, canonical_path_for_mode, map_mode_from_path, normalize_heat_selected_season_id,
        should_wait_for_history_probe,
    };
    use super::{SHEET_DISMISS_DRAG_PX, SheetDrag, sidebar_transform};
    use sequoia_shared::history::{HistoryHeatMeta, HistoryHeatSeasonWindow};

    #[test]
    fn heat_legend_labels_the_uneven_quartile_counts_of_a_skewed_period() {
        let ordinary = [10, 20, 30, 40, 50]
            .into_iter()
            .flat_map(|level| [level; 8]);
        let scale = HeatScale::new(ordinary.chain([200]));
        let stops = heat_legend_stops(&scale);
        let labels: Vec<&str> = stops.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(labels, ["10", "20", "30", "40", "200"]);
        // Eight of 41 territories at each ordinary level, one at 200.
        let shares: Vec<f64> = stops.iter().map(|(share, _)| *share).collect();
        assert_eq!(shares, [8.0, 16.0, 24.0, 32.0, 41.0].map(|at| at / 41.0));
    }

    #[test]
    fn heat_legend_puts_a_single_or_uniform_count_where_it_renders() {
        for scale in [HeatScale::new([7; 12]), HeatScale::new([0, 7, 0])] {
            let stops = heat_legend_stops(&scale);
            assert_eq!(stops, [(1.0, "7".to_string())]);
            // The stop sits where the map colours that count.
            assert_eq!(scale.intensity(7), stops[0].0);
        }
        assert!(heat_legend_stops(&HeatScale::new([0, 0])).is_empty());
        assert!(heat_legend_stops(&HeatScale::new([])).is_empty());
    }

    #[test]
    fn heat_legend_labels_a_tied_count_once_at_the_colour_it_renders() {
        // Three territories at 1 fill the first three quarters; all render at 0.75.
        assert_eq!(
            heat_legend_stops(&HeatScale::new([1, 1, 1, 2])),
            [(0.75, "1".to_string()), (1.0, "2".to_string())]
        );
        // A large tie in the middle is one label, at 25 of 30 territories.
        let middle = HeatScale::new([1; 5].into_iter().chain([2; 20]).chain([3; 5]));
        assert_eq!(
            heat_legend_stops(&middle),
            [
                (5.0 / 30.0, "1".to_string()),
                (25.0 / 30.0, "2".to_string()),
                (1.0, "3".to_string())
            ]
        );
    }

    #[test]
    fn heat_legend_labels_each_count_once_where_the_map_colours_it() {
        let periods: [&[u64]; 6] = [
            &[1, 1, 1, 2],
            &[5, 5, 9],
            &[3, 3, 3, 3, 8, 8, 8, 8, 13],
            &[0, 0, 4, 4, 4, 6],
            &[2, 7, 7, 7, 7, 7, 7, 7, 11, 40],
            &[4, 16, 16, 31, 51, 51, 51, 209],
        ];
        for counts in periods {
            let scale = HeatScale::new(counts.iter().copied());
            let stops = heat_legend_stops(&scale);
            let mut previous: Option<f64> = None;
            for (share, label) in &stops {
                let count: u64 = label.parse().expect("exact below ten thousand");
                assert_eq!(scale.intensity(count), *share, "{counts:?}: {label}");
                assert!(previous.is_none_or(|p| p < *share), "{counts:?}: {stops:?}");
                previous = Some(*share);
            }
            assert_eq!(
                stops.last().map(|(share, _)| *share),
                Some(1.0),
                "{counts:?}"
            );
        }
    }

    #[test]
    fn heat_legend_staggers_a_count_that_would_touch_its_neighbour() {
        // Ninety-nine territories at 1 and one at 2: distinct counts at 0.99 and 1.
        let concentrated = HeatScale::new([1; 99].into_iter().chain([2]));
        let stops = heat_legend_stops(&concentrated);
        assert_eq!(stops, [(0.99, "1".to_string()), (1.0, "2".to_string())]);
        assert_eq!(heat_legend_rows(&stops), [0, 1]);
    }

    #[test]
    fn heat_legend_keeps_spread_labels_on_one_row() {
        let season = [4, 16, 31, 51, 209].map(|count| count.to_string());
        let stops: Vec<(f64, String)> = [0.014, 0.263, 0.503, 0.753, 1.0]
            .into_iter()
            .zip(season)
            .collect();
        assert_eq!(heat_legend_rows(&stops), [0; 5]);
        assert_eq!(heat_legend_rows(&[(1.0, "7".to_string())]), [0]);
        assert!(heat_legend_rows(&[]).is_empty());
    }

    #[test]
    fn heat_legend_labels_on_one_row_never_touch() {
        let periods: [Vec<u64>; 4] = [
            [1; 99].into_iter().chain([2]).collect(),
            [1].into_iter().chain([2; 98]).chain([3]).collect(),
            // Long labels bunched around the first quarter.
            [9_999; 24]
                .into_iter()
                .chain([10_000])
                .chain([12_500; 25])
                .chain([30_000; 25])
                .chain([44_167; 25])
                .collect(),
            [744, 2_246, 2_246, 3_280, 6_156, 6_156, 44_167].to_vec(),
        ];
        for counts in periods {
            let stops = heat_legend_stops(&HeatScale::new(counts.iter().copied()));
            let rows = heat_legend_rows(&stops);
            for row in 0..stops.len() {
                let mut end: Option<f64> = None;
                for ((share, label), _) in stops.iter().zip(&rows).filter(|(_, r)| **r == row) {
                    let len = label.len() as f64;
                    let start = share * (HEAT_LEGEND_BAR_CHARS - len);
                    assert!(
                        end.is_none_or(|end| start - end >= 1.0),
                        "{stops:?} {rows:?}"
                    );
                    end = Some(start + len);
                }
            }
        }
    }

    #[test]
    fn heat_counts_are_exact_until_ten_thousand() {
        assert_eq!(format_heat_count(9_999), "9999");
        assert_eq!(format_heat_count(44_167), "44.2k");
        assert_eq!(
            format_heat_count(u64::MAX),
            format!("{:.1}k", u64::MAX as f64 / 1000.0)
        );
    }

    #[test]
    fn normalize_heat_selected_season_id_keeps_valid_saved_selection() {
        let meta = HistoryHeatMeta {
            latest_season_id: Some(31),
            seasons: vec![
                HistoryHeatSeasonWindow {
                    season_id: 31,
                    start: "2026-01-01T00:00:00Z".to_string(),
                    end: "2026-01-08T00:00:00Z".to_string(),
                    is_current: true,
                },
                HistoryHeatSeasonWindow {
                    season_id: 30,
                    start: "2025-12-24T00:00:00Z".to_string(),
                    end: "2025-12-31T00:00:00Z".to_string(),
                    is_current: false,
                },
            ],
            all_time_earliest: None,
            retention_days: 30,
            season_fallback_days: 60,
        };

        assert_eq!(normalize_heat_selected_season_id(&meta, Some(30)), Some(30));
    }

    #[test]
    fn normalize_heat_selected_season_id_replaces_invalid_saved_selection() {
        let meta = HistoryHeatMeta {
            latest_season_id: Some(31),
            seasons: vec![HistoryHeatSeasonWindow {
                season_id: 31,
                start: "2026-01-01T00:00:00Z".to_string(),
                end: "2026-01-08T00:00:00Z".to_string(),
                is_current: true,
            }],
            all_time_earliest: None,
            retention_days: 30,
            season_fallback_days: 60,
        };

        assert_eq!(normalize_heat_selected_season_id(&meta, Some(999)), None);
        // Latest stays Latest, following the next season when it starts.
        assert_eq!(normalize_heat_selected_season_id(&meta, None), None);
    }

    #[test]
    fn normalize_heat_selected_season_id_returns_none_without_valid_latest() {
        let meta = HistoryHeatMeta {
            latest_season_id: Some(31),
            seasons: vec![],
            all_time_earliest: None,
            retention_days: 30,
            season_fallback_days: 60,
        };

        assert_eq!(normalize_heat_selected_season_id(&meta, Some(999)), None);
    }

    #[test]
    fn map_mode_helpers_keep_canonical_history_paths() {
        assert_eq!(map_mode_from_path("/"), MapMode::Live);
        assert_eq!(map_mode_from_path("/history"), MapMode::History);
        assert_eq!(map_mode_from_path("/history/"), MapMode::History);
        assert_eq!(map_mode_from_path("/history/extra"), MapMode::History);
        assert_eq!(canonical_path_for_mode(MapMode::Live), "/");
        assert_eq!(canonical_path_for_mode(MapMode::History), "/history");
    }

    #[test]
    fn history_probe_waits_only_before_first_probe_completion() {
        assert!(should_wait_for_history_probe(false, 0));
        assert!(!should_wait_for_history_probe(false, 1));
        assert!(!should_wait_for_history_probe(true, 0));
    }

    #[test]
    fn mobile_sheet_rests_where_its_open_state_puts_it() {
        // A drag only shows while the sheet is open on mobile; when it ends the open sheet
        // is back in place, not left at the stylesheet's closed position.
        assert_eq!(
            sidebar_transform(true, true, Some(30.0)),
            "translateY(30px)"
        );
        assert_eq!(sidebar_transform(true, true, None), "translateY(0)");
        assert_eq!(
            sidebar_transform(true, false, Some(30.0)),
            "translateY(100%)"
        );
        assert_eq!(sidebar_transform(false, true, Some(30.0)), "translateX(0)");
        assert_eq!(sidebar_transform(false, false, None), "translateX(100%)");
    }

    #[test]
    fn sheet_drags_follow_their_pointer_down_only() {
        let drag = SheetDrag::new(7, 500.0);
        assert_eq!(drag.moved_to(470.0).offset, 0.0);
        assert_eq!(drag.moved_to(530.0).offset, 30.0);
        assert!(!drag.moved_to(500.0 + SHEET_DISMISS_DRAG_PX).dismisses());
        assert!(drag.moved_to(501.0 + SHEET_DISMISS_DRAG_PX).dismisses());
        assert_eq!(drag.moved_to(530.0).pointer_id, 7);
    }
}
