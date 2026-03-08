use js_sys::{Function, Reflect, encode_uri_component};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) const DEFAULT_SIDEBAR_WIDTH: f64 = 420.0;
pub(crate) const SIDEBAR_WIDTH_MIN: f64 = 320.0;
pub(crate) const SIDEBAR_WIDTH_MAX: f64 = 620.0;
const LEGACY_DEFAULT_SIDEBAR_WIDTH: f64 = 380.0;

pub(crate) fn clamp_sidebar_width(value: f64) -> f64 {
    value.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
}

fn migrate_sidebar_width(value: f64) -> f64 {
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

fn set_loading_shell_step(step: &str) {
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

fn remove_loading_shell() {
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
use sequoia_shared::{Region, Resources, SeasonScalarSample, TerritoryChange, TreasuryLevel};

/// Newtype wrappers to give `hovered` and `selected` distinct types for Leptos context.
/// (Both are `RwSignal<Option<String>>` — without wrappers, `provide_context` overwrites one.)
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
pub(crate) struct ManualSrScalar(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct AutoSrScalarEnabled(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowLeaderboardSrGain(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowLeaderboardSrValue(pub RwSignal<bool>);
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
pub(crate) struct SidebarWidth(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarTransient(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarIndex(pub RwSignal<usize>);
#[derive(Clone, Copy)]
pub(crate) struct SidebarItems(pub RwSignal<Vec<String>>);
#[derive(Clone, Copy)]
pub(crate) struct ResetSettingsTrigger(pub RwSignal<u64>);
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
}

#[derive(Clone, Copy)]
pub(crate) struct GuildOnlineData(pub RwSignal<HashMap<String, GuildOnlineInfo>>);
#[derive(Clone, Copy)]
pub(crate) struct ShowLeaderboardTerritoryCount(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct ShowLeaderboardOnline(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct LeaderboardSortBySr(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatModeEnabled(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatLiveSourceSetting(pub RwSignal<HeatLiveSource>);
#[derive(Clone, Copy)]
pub(crate) struct HeatHistoryBasisSetting(pub RwSignal<HeatHistoryBasis>);
#[derive(Clone, Copy)]
pub(crate) struct HeatSelectedSeasonId(pub RwSignal<Option<i32>>);
#[derive(Clone, Copy)]
pub(crate) struct HeatEntriesByTerritory(pub RwSignal<HashMap<String, u64>>);
#[derive(Clone, Copy)]
pub(crate) struct HeatMaxTakeCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct HeatFallbackApplied(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct HeatWindowLabel(pub RwSignal<String>);
#[derive(Clone, Copy)]
pub(crate) struct HeatMetaState(pub RwSignal<Option<HistoryHeatMeta>>);

pub(crate) const MOBILE_BREAKPOINT: f64 = 768.0;
const GUILD_ONLINE_POLL_INTERVAL_SECS: u64 = 120;
const GUILD_ONLINE_BOOTSTRAP_RETRY_SECS: u64 = 3;
const GUILD_ONLINE_ERROR_RETRY_SECS: u64 = 15;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapMode {
    Live,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeatLiveSource {
    Season,
    AllTime,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeatHistoryBasis {
    SeasonCumulative,
    AllTimeCumulative,
}

#[derive(Clone, Copy)]
pub(crate) struct CurrentMode(pub RwSignal<MapMode>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryTimestamp(pub RwSignal<Option<i64>>);
#[derive(Clone, Copy)]
pub(crate) struct PlaybackActive(pub RwSignal<bool>);
#[derive(Clone, Copy)]
pub(crate) struct PlaybackSpeed(pub RwSignal<f64>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBoundsSignal(pub RwSignal<Option<(i64, i64)>>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryAvailable(pub RwSignal<bool>);
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
pub(crate) struct LiveHandoffResyncCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct SseSeqGapDetectedCount(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct HistoryBufferSizeMax(pub RwSignal<usize>);
#[derive(Clone, Copy)]
pub(crate) struct LiveSeasonScalarSample(pub RwSignal<Option<SeasonScalarSample>>);
#[derive(Clone, Copy)]
pub(crate) struct HistorySeasonScalarSample(pub RwSignal<Option<SeasonScalarSample>>);
#[derive(Clone, Copy)]
pub(crate) struct HistorySeasonLeaderboard(pub RwSignal<Option<Vec<HistoryGuildSrEntry>>>);

#[derive(Clone, Debug)]
pub(crate) struct BufferedUpdate {
    pub seq: u64,
    pub changes: Vec<TerritoryChange>,
}

pub(crate) type TerritoryGeometry = (Region, Resources, Vec<String>);
pub(crate) type TerritoryGeometryMap = HashMap<String, TerritoryGeometry>;
pub(crate) type GuildColorMap = HashMap<String, (u8, u8, u8)>;

/// Immutable snapshot of territory geometry captured when entering history mode.
/// Prevents history/playback operations from degrading geometry data.
#[derive(Clone, Copy)]
pub(crate) struct TerritoryGeometryStore(pub StoredValue<TerritoryGeometryMap>);
#[derive(Clone, Copy)]
pub(crate) struct GuildColorStore(pub StoredValue<GuildColorMap>);

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum NameColor {
    White,  // rgba(220, 218, 210, 0.88) — current default
    Guild,  // per-territory guild color (brightened), same as tag line
    Gold,   // rgba(245, 197, 66, 0.88) — matches app accent
    Copper, // rgba(181, 103, 39, 0.88) — warm copper
    Muted,  // rgba(120, 116, 112, 0.78) — subtle/subdued
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FontRendererMode {
    #[default]
    Auto,
    Classic,
    Dynamic,
    ExperimentalGpu,
}

use gloo_storage::Storage;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SettingsV2 {
    show_connections: bool,
    abbreviate_names: bool,
    show_countdown: bool,
    granular_map_time: bool,
    #[serde(default = "default_true")]
    compound_map_time: bool,
    show_names: bool,
    thick_cooldown_borders: bool,
    bold_connections: bool,
    #[serde(default = "default_connection_opacity_scale")]
    connection_opacity_scale: f64,
    #[serde(default = "default_connection_thickness_scale")]
    connection_thickness_scale: f64,
    sidebar_open: bool,
    resource_highlight: bool,
    show_resource_icons: bool,
    #[serde(default = "default_true")]
    show_territory_ornaments: bool,
    #[serde(default = "default_manual_sr_scalar")]
    manual_sr_scalar: f64,
    auto_sr_scalar_enabled: bool,
    show_leaderboard_sr_gain: bool,
    #[serde(default)]
    show_leaderboard_sr_value: bool,
    #[serde(default = "default_true")]
    show_leaderboard_territory_count: bool,
    #[serde(default = "default_true")]
    show_leaderboard_online: bool,
    leaderboard_sort_by_sr: bool,
    #[serde(default)]
    heat_mode_enabled: bool,
    #[serde(default = "default_heat_live_source")]
    heat_live_source: HeatLiveSource,
    #[serde(default = "default_heat_history_basis")]
    heat_history_basis: HeatHistoryBasis,
    #[serde(default)]
    heat_selected_season_id: Option<i32>,
    #[serde(default)]
    readable_font: bool,
    #[serde(default = "default_true")]
    show_minimap: bool,
    #[serde(default = "default_name_color")]
    name_color: NameColor,
    #[serde(default = "default_tag_color")]
    tag_color: NameColor,
    #[serde(default = "default_label_scale_master")]
    label_scale_master: f64,
    #[serde(default = "default_label_scale_static_tag")]
    label_scale_static: f64,
    #[serde(default)]
    label_scale_static_name: Option<f64>,
    #[serde(default = "default_label_scale_group")]
    label_scale_dynamic: f64,
    #[serde(default = "default_label_scale_group")]
    label_scale_icons: f64,
    #[serde(default = "default_sidebar_width")]
    sidebar_width: f64,
}

const fn default_name_color() -> NameColor {
    NameColor::Guild
}

const fn default_tag_color() -> NameColor {
    NameColor::Guild
}

const fn default_heat_live_source() -> HeatLiveSource {
    HeatLiveSource::AllTime
}

const fn default_heat_history_basis() -> HeatHistoryBasis {
    HeatHistoryBasis::SeasonCumulative
}

const fn default_true() -> bool {
    true
}

const fn default_manual_sr_scalar() -> f64 {
    1.5
}

const fn default_connection_opacity_scale() -> f64 {
    DEFAULT_CONNECTION_OPACITY_SCALE
}

const fn default_connection_thickness_scale() -> f64 {
    DEFAULT_CONNECTION_THICKNESS_SCALE
}

const fn default_label_scale_master() -> f64 {
    DEFAULT_LABEL_SCALE_MASTER
}

const fn default_label_scale_group() -> f64 {
    DEFAULT_LABEL_SCALE_GROUP
}

const fn default_label_scale_static_tag() -> f64 {
    DEFAULT_LABEL_SCALE_STATIC_TAG
}

const fn default_label_scale_static_name() -> f64 {
    DEFAULT_LABEL_SCALE_STATIC_NAME
}

const fn default_sidebar_width() -> f64 {
    DEFAULT_SIDEBAR_WIDTH
}

pub(crate) const DEFAULT_LABEL_SCALE_MASTER: f64 = 1.0;
pub(crate) const DEFAULT_LABEL_SCALE_GROUP: f64 = 1.0;
pub(crate) const DEFAULT_LABEL_SCALE_STATIC_TAG: f64 = 1.0;
pub(crate) const DEFAULT_LABEL_SCALE_STATIC_NAME: f64 = 1.0;
pub(crate) const DEFAULT_CONNECTION_OPACITY_SCALE: f64 = 1.0;
pub(crate) const DEFAULT_CONNECTION_THICKNESS_SCALE: f64 = 1.0;
pub(crate) const CONNECTION_OPACITY_SCALE_MIN: f64 = 0.60;
pub(crate) const CONNECTION_OPACITY_SCALE_MAX: f64 = 2.50;
pub(crate) const CONNECTION_THICKNESS_SCALE_MIN: f64 = 0.70;
pub(crate) const CONNECTION_THICKNESS_SCALE_MAX: f64 = 2.50;
pub(crate) const LABEL_SCALE_MASTER_MIN: f64 = 1.0;
pub(crate) const LABEL_SCALE_MASTER_MAX: f64 = 2.25;
pub(crate) const LABEL_SCALE_GROUP_MIN: f64 = 0.60;
pub(crate) const LABEL_SCALE_GROUP_MAX: f64 = 1.80;

pub(crate) fn clamp_connection_opacity_scale(value: f64) -> f64 {
    value.clamp(CONNECTION_OPACITY_SCALE_MIN, CONNECTION_OPACITY_SCALE_MAX)
}

pub(crate) fn clamp_connection_thickness_scale(value: f64) -> f64 {
    value.clamp(
        CONNECTION_THICKNESS_SCALE_MIN,
        CONNECTION_THICKNESS_SCALE_MAX,
    )
}

pub(crate) fn clamp_label_scale_master(value: f64) -> f64 {
    value.clamp(LABEL_SCALE_MASTER_MIN, LABEL_SCALE_MASTER_MAX)
}

pub(crate) fn clamp_label_scale_group(value: f64) -> f64 {
    value.clamp(LABEL_SCALE_GROUP_MIN, LABEL_SCALE_GROUP_MAX)
}

impl Default for SettingsV2 {
    fn default() -> Self {
        Self {
            show_connections: true,
            abbreviate_names: true,
            show_countdown: false,
            granular_map_time: false,
            compound_map_time: true,
            show_names: false,
            thick_cooldown_borders: true,
            bold_connections: false,
            connection_opacity_scale: default_connection_opacity_scale(),
            connection_thickness_scale: default_connection_thickness_scale(),
            sidebar_open: false,
            resource_highlight: false,
            show_resource_icons: false,
            show_territory_ornaments: true,
            manual_sr_scalar: default_manual_sr_scalar(),
            auto_sr_scalar_enabled: false,
            show_leaderboard_sr_gain: false,
            show_leaderboard_sr_value: false,
            show_leaderboard_territory_count: true,
            show_leaderboard_online: true,
            leaderboard_sort_by_sr: false,
            heat_mode_enabled: false,
            heat_live_source: default_heat_live_source(),
            heat_history_basis: default_heat_history_basis(),
            heat_selected_season_id: None,
            readable_font: false,
            show_minimap: true,
            name_color: default_name_color(),
            tag_color: default_tag_color(),
            label_scale_master: default_label_scale_master(),
            label_scale_static: default_label_scale_static_tag(),
            label_scale_static_name: Some(default_label_scale_static_name()),
            label_scale_dynamic: default_label_scale_group(),
            label_scale_icons: default_label_scale_group(),
            sidebar_width: default_sidebar_width(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct LegacySettings {
    show_connections: bool,
    abbreviate_names: bool,
    show_countdown: bool,
    granular_map_time: bool,
    #[serde(default = "default_true")]
    compound_map_time: bool,
    show_names: bool,
    thick_cooldown_borders: bool,
    bold_names: bool,
    bold_tags: bool,
    thick_tag_outline: bool,
    thick_name_outline: bool,
    readable_font: bool,
    font_renderer_mode: Option<FontRendererMode>,
    #[serde(default, rename = "experimental_gpu_labels")]
    legacy_experimental_gpu_labels: Option<bool>,
    bold_connections: bool,
    name_color: NameColor,
    sidebar_open: bool,
    resource_highlight: bool,
    show_resource_icons: bool,
    #[serde(default = "default_manual_sr_scalar")]
    manual_sr_scalar: f64,
    auto_sr_scalar_enabled: bool,
    show_leaderboard_sr_gain: bool,
}

impl Default for LegacySettings {
    fn default() -> Self {
        Self {
            show_connections: true,
            abbreviate_names: true,
            show_countdown: false,
            granular_map_time: false,
            compound_map_time: true,
            show_names: false,
            thick_cooldown_borders: true,
            bold_names: false,
            bold_tags: false,
            thick_tag_outline: false,
            thick_name_outline: false,
            readable_font: false,
            font_renderer_mode: Some(FontRendererMode::Auto),
            legacy_experimental_gpu_labels: None,
            bold_connections: false,
            name_color: NameColor::White,
            sidebar_open: false,
            resource_highlight: false,
            show_resource_icons: false,
            manual_sr_scalar: default_manual_sr_scalar(),
            auto_sr_scalar_enabled: false,
            show_leaderboard_sr_gain: false,
        }
    }
}

impl From<LegacySettings> for SettingsV2 {
    fn from(value: LegacySettings) -> Self {
        Self {
            show_connections: value.show_connections,
            abbreviate_names: value.abbreviate_names,
            show_countdown: value.show_countdown,
            granular_map_time: value.granular_map_time,
            compound_map_time: value.compound_map_time,
            show_names: value.show_names,
            thick_cooldown_borders: value.thick_cooldown_borders,
            bold_connections: value.bold_connections,
            connection_opacity_scale: default_connection_opacity_scale(),
            connection_thickness_scale: default_connection_thickness_scale(),
            sidebar_open: value.sidebar_open,
            resource_highlight: value.resource_highlight,
            show_resource_icons: value.show_resource_icons,
            show_territory_ornaments: true,
            manual_sr_scalar: value.manual_sr_scalar,
            auto_sr_scalar_enabled: value.auto_sr_scalar_enabled,
            show_leaderboard_sr_gain: value.show_leaderboard_sr_gain,
            show_leaderboard_sr_value: false,
            show_leaderboard_territory_count: true,
            show_leaderboard_online: true,
            leaderboard_sort_by_sr: false,
            heat_mode_enabled: false,
            heat_live_source: default_heat_live_source(),
            heat_history_basis: default_heat_history_basis(),
            heat_selected_season_id: None,
            readable_font: value.readable_font,
            show_minimap: true,
            name_color: value.name_color,
            tag_color: default_tag_color(),
            label_scale_master: default_label_scale_master(),
            label_scale_static: default_label_scale_static_tag(),
            label_scale_static_name: Some(default_label_scale_static_name()),
            label_scale_dynamic: default_label_scale_group(),
            label_scale_icons: default_label_scale_group(),
            sidebar_width: default_sidebar_width(),
        }
    }
}

fn load_settings_v2() -> SettingsV2 {
    if let Ok(saved) = gloo_storage::LocalStorage::get::<SettingsV2>("sequoia_settings_v2") {
        return saved;
    }
    if let Ok(legacy) = gloo_storage::LocalStorage::get::<LegacySettings>("sequoia_settings") {
        return legacy.into();
    }
    SettingsV2::default()
}

use crate::canvas::MapCanvas;
use crate::colors::rgba_css;
use crate::heat::{self, HeatFetchInput};
use crate::history;
use crate::icons::{self, ResourceAtlas};
use crate::season_scalar;
use crate::sidebar::Sidebar;
use crate::sse::{self, ConnectionStatus};
use crate::territory::ClientTerritoryMap;
use crate::tiles::{self, LoadedTile};
use crate::time_format::format_hms;
use crate::timeline::Timeline;
use crate::viewport::Viewport;

/// Format a resource value for compact display (e.g. 9000 -> "9.0k").
fn format_resource_compact(val: i32) -> String {
    if val >= 1000 {
        format!("{:.1}k", val as f64 / 1000.0)
    } else {
        format!("{val}")
    }
}

fn tooltip_resource_items(
    values: &Resources,
    double_source: &Resources,
) -> Vec<(i32, bool, &'static str, &'static str)> {
    vec![
        (
            values.emeralds,
            double_source.has_double_emeralds(),
            "emerald",
            "Emeralds",
        ),
        (values.ore, double_source.has_double_ore(), "ore", "Ore"),
        (
            values.crops,
            double_source.has_double_crops(),
            "crops",
            "Crops",
        ),
        (values.fish, double_source.has_double_fish(), "fish", "Fish"),
        (values.wood, double_source.has_double_wood(), "wood", "Wood"),
    ]
}

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

fn normalize_heat_selected_season_id(meta: &HistoryHeatMeta, selected: Option<i32>) -> Option<i32> {
    let has_season = |season_id: i32| {
        meta.seasons
            .iter()
            .any(|window| window.season_id == season_id)
    };
    let latest_valid = meta
        .latest_season_id
        .filter(|season_id| has_season(*season_id));
    match selected {
        Some(season_id) if has_season(season_id) => Some(season_id),
        _ => latest_valid,
    }
}
/// Root application component. Provides global reactive signals via context.
#[component]
pub fn App() -> impl IntoView {
    // Global signals
    let territories: RwSignal<ClientTerritoryMap> = RwSignal::new(Default::default());
    let viewport: RwSignal<Viewport> = RwSignal::new(Viewport::default());
    let hovered: RwSignal<Option<String>> = RwSignal::new(None);
    let selected: RwSignal<Option<String>> = RwSignal::new(None);
    let search_query: RwSignal<String> = RwSignal::new(String::new());
    let connection: RwSignal<ConnectionStatus> = RwSignal::new(ConnectionStatus::Connecting);
    let mouse_pos: RwSignal<(f64, f64)> = RwSignal::new((0.0, 0.0));
    let loaded_tiles: RwSignal<Vec<LoadedTile>> = RwSignal::new(Vec::new());
    let loaded_icons: RwSignal<Option<ResourceAtlas>> = RwSignal::new(None);
    // Epoch-second tick — drives cooldown countdown updates across canvas, tooltip, sidebar
    let tick: RwSignal<i64> = RwSignal::new(chrono::Utc::now().timestamp());
    let saved = load_settings_v2();
    let show_connections: RwSignal<bool> = RwSignal::new(saved.show_connections);
    let abbreviate_names: RwSignal<bool> = RwSignal::new(saved.abbreviate_names);
    let show_countdown: RwSignal<bool> = RwSignal::new(saved.show_countdown);
    let show_granular_map_time: RwSignal<bool> = RwSignal::new(saved.granular_map_time);
    let show_compound_map_time: RwSignal<bool> = RwSignal::new(saved.compound_map_time);
    let show_names: RwSignal<bool> = RwSignal::new(saved.show_names);
    let thick_cooldown_borders: RwSignal<bool> = RwSignal::new(saved.thick_cooldown_borders);
    let bold_connections: RwSignal<bool> = RwSignal::new(saved.bold_connections);
    let connection_opacity_scale: RwSignal<f64> = RwSignal::new(clamp_connection_opacity_scale(
        saved.connection_opacity_scale,
    ));
    let connection_thickness_scale: RwSignal<f64> = RwSignal::new(
        clamp_connection_thickness_scale(saved.connection_thickness_scale),
    );
    let resource_highlight: RwSignal<bool> = RwSignal::new(saved.resource_highlight);
    let show_resource_icons: RwSignal<bool> = RwSignal::new(saved.show_resource_icons);
    let show_territory_ornaments: RwSignal<bool> = RwSignal::new(saved.show_territory_ornaments);
    let manual_sr_scalar: RwSignal<f64> =
        RwSignal::new(season_scalar::clamp_manual_scalar(saved.manual_sr_scalar));
    let auto_sr_scalar_enabled: RwSignal<bool> = RwSignal::new(saved.auto_sr_scalar_enabled);
    let show_leaderboard_sr_gain: RwSignal<bool> = RwSignal::new(saved.show_leaderboard_sr_gain);
    let show_leaderboard_sr_value: RwSignal<bool> = RwSignal::new(saved.show_leaderboard_sr_value);
    let name_color: RwSignal<NameColor> = RwSignal::new(saved.name_color);
    let tag_color: RwSignal<NameColor> = RwSignal::new(saved.tag_color);
    let readable_font: RwSignal<bool> = RwSignal::new(saved.readable_font);
    let show_minimap: RwSignal<bool> = RwSignal::new(saved.show_minimap);
    let label_scale_master: RwSignal<f64> =
        RwSignal::new(clamp_label_scale_master(saved.label_scale_master));
    let label_scale_static: RwSignal<f64> =
        RwSignal::new(clamp_label_scale_group(saved.label_scale_static));
    let label_scale_static_name: RwSignal<f64> = RwSignal::new(clamp_label_scale_group(
        saved
            .label_scale_static_name
            .unwrap_or(saved.label_scale_static),
    ));
    let label_scale_dynamic: RwSignal<f64> =
        RwSignal::new(clamp_label_scale_group(saved.label_scale_dynamic));
    let label_scale_icons: RwSignal<f64> =
        RwSignal::new(clamp_label_scale_group(saved.label_scale_icons));
    let sidebar_width: RwSignal<f64> = RwSignal::new(clamp_sidebar_width(migrate_sidebar_width(
        saved.sidebar_width,
    )));
    let sidebar_open: RwSignal<bool> = RwSignal::new(saved.sidebar_open);
    let sidebar_transient: RwSignal<bool> = RwSignal::new(false);
    let sidebar_ready: RwSignal<bool> = RwSignal::new(false);
    let sidebar_loaded: RwSignal<bool> = RwSignal::new(saved.sidebar_open);
    let sidebar_index: RwSignal<usize> = RwSignal::new(0);
    let sidebar_items: RwSignal<Vec<String>> = RwSignal::new(Vec::new());
    let reset_settings_trigger: RwSignal<u64> = RwSignal::new(0);
    let show_settings: RwSignal<bool> = RwSignal::new(false);
    // Live-first boot: defer non-essential work (tiles/history checks/icons)
    // until we have initial territory data and a short settle window.
    let deferred_boot_ready: RwSignal<bool> = RwSignal::new(false);
    let deferred_boot_timer_set: RwSignal<bool> = RwSignal::new(false);
    let tile_fetch_scheduled: RwSignal<bool> = RwSignal::new(false);
    let icons_loaded: RwSignal<bool> = RwSignal::new(false);
    let loading_shell_removed: RwSignal<bool> = RwSignal::new(false);

    // Mobile detection
    let is_mobile: RwSignal<bool> = RwSignal::new(canvas_dimensions().0 < MOBILE_BREAKPOINT);
    let peek_territory: RwSignal<Option<String>> = RwSignal::new(None);
    let selected_guild: RwSignal<Option<String>> = RwSignal::new(None);
    let detail_return_guild: RwSignal<Option<String>> = RwSignal::new(None);
    let guild_online_data: RwSignal<HashMap<String, GuildOnlineInfo>> =
        RwSignal::new(HashMap::new());
    let show_leaderboard_territory_count: RwSignal<bool> =
        RwSignal::new(saved.show_leaderboard_territory_count);
    let show_leaderboard_online: RwSignal<bool> = RwSignal::new(saved.show_leaderboard_online);
    let leaderboard_sort_by_sr: RwSignal<bool> = RwSignal::new(saved.leaderboard_sort_by_sr);
    let heat_mode_enabled: RwSignal<bool> = RwSignal::new(saved.heat_mode_enabled);
    let heat_live_source: RwSignal<HeatLiveSource> = RwSignal::new(saved.heat_live_source);
    let heat_history_basis: RwSignal<HeatHistoryBasis> = RwSignal::new(saved.heat_history_basis);
    let heat_selected_season_id: RwSignal<Option<i32>> =
        RwSignal::new(saved.heat_selected_season_id);
    let heat_entries_by_territory: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    let heat_max_take_count: RwSignal<u64> = RwSignal::new(0);
    let heat_fallback_applied: RwSignal<bool> = RwSignal::new(false);
    let heat_window_label: RwSignal<String> = RwSignal::new(String::new());
    let heat_meta: RwSignal<Option<HistoryHeatMeta>> = RwSignal::new(None);
    let heat_refresh_nonce: RwSignal<u64> = RwSignal::new(0);

    // History mode signals
    let map_mode: RwSignal<MapMode> = RwSignal::new(MapMode::Live);
    let history_timestamp: RwSignal<Option<i64>> = RwSignal::new(None);
    let playback_active: RwSignal<bool> = RwSignal::new(false);
    let playback_speed: RwSignal<f64> = RwSignal::new(10.0);
    let history_bounds: RwSignal<Option<(i64, i64)>> = RwSignal::new(None);
    let history_available: RwSignal<bool> = RwSignal::new(false);
    let history_fetch_nonce: RwSignal<u64> = RwSignal::new(0);
    let last_live_seq: RwSignal<Option<u64>> = RwSignal::new(None);
    let history_buffered_updates: RwSignal<Vec<BufferedUpdate>> = RwSignal::new(Vec::new());
    let history_buffer_mode_active: RwSignal<bool> = RwSignal::new(false);
    let needs_live_resync: RwSignal<bool> = RwSignal::new(false);
    let live_resync_in_flight: RwSignal<bool> = RwSignal::new(false);
    let live_handoff_resync_count: RwSignal<u64> = RwSignal::new(0);
    let sse_seq_gap_detected_count: RwSignal<u64> = RwSignal::new(0);
    let history_buffer_size_max: RwSignal<usize> = RwSignal::new(0);
    let live_season_scalar_sample: RwSignal<Option<SeasonScalarSample>> = RwSignal::new(None);
    let history_season_scalar_sample: RwSignal<Option<SeasonScalarSample>> = RwSignal::new(None);
    let history_season_leaderboard: RwSignal<Option<Vec<HistoryGuildSrEntry>>> =
        RwSignal::new(None);
    let territory_geometry: StoredValue<TerritoryGeometryMap> = StoredValue::new(HashMap::new());
    let guild_colors: StoredValue<GuildColorMap> = StoredValue::new(HashMap::new());

    // Provide via context so children can access
    provide_context(territories);
    provide_context(viewport);
    provide_context(Hovered(hovered));
    provide_context(Selected(selected));
    provide_context(search_query);
    provide_context(connection);
    provide_context(mouse_pos);
    provide_context(loaded_tiles);
    provide_context(loaded_icons);
    provide_context(tick);
    provide_context(show_connections);
    provide_context(AbbreviateNames(abbreviate_names));
    provide_context(ShowCountdown(show_countdown));
    provide_context(ShowGranularMapTime(show_granular_map_time));
    provide_context(ShowCompoundMapTime(show_compound_map_time));
    provide_context(ShowNames(show_names));
    provide_context(ThickCooldownBorders(thick_cooldown_borders));
    provide_context(BoldConnections(bold_connections));
    provide_context(ConnectionOpacityScale(connection_opacity_scale));
    provide_context(ConnectionThicknessScale(connection_thickness_scale));
    provide_context(ResourceHighlight(resource_highlight));
    provide_context(ShowResourceIcons(show_resource_icons));
    provide_context(ShowTerritoryOrnaments(show_territory_ornaments));
    provide_context(ManualSrScalar(manual_sr_scalar));
    provide_context(AutoSrScalarEnabled(auto_sr_scalar_enabled));
    provide_context(ShowLeaderboardSrGain(show_leaderboard_sr_gain));
    provide_context(ShowLeaderboardSrValue(show_leaderboard_sr_value));
    provide_context(NameColorSetting(name_color));
    provide_context(TagColorSetting(tag_color));
    provide_context(ReadableFont(readable_font));
    provide_context(ShowMinimap(show_minimap));
    provide_context(LabelScaleMaster(label_scale_master));
    provide_context(LabelScaleStatic(label_scale_static));
    provide_context(LabelScaleStaticName(label_scale_static_name));
    provide_context(LabelScaleDynamic(label_scale_dynamic));
    provide_context(LabelScaleIcons(label_scale_icons));
    provide_context(SidebarOpen(sidebar_open));
    provide_context(SidebarWidth(sidebar_width));
    provide_context(SidebarTransient(sidebar_transient));
    provide_context(SidebarIndex(sidebar_index));
    provide_context(SidebarItems(sidebar_items));
    provide_context(ResetSettingsTrigger(reset_settings_trigger));
    provide_context(ShowSettings(show_settings));
    provide_context(CurrentMode(map_mode));
    provide_context(HistoryTimestamp(history_timestamp));
    provide_context(PlaybackActive(playback_active));
    provide_context(PlaybackSpeed(playback_speed));
    provide_context(HistoryBoundsSignal(history_bounds));
    provide_context(HistoryAvailable(history_available));
    provide_context(HistoryFetchNonce(history_fetch_nonce));
    provide_context(LastLiveSeq(last_live_seq));
    provide_context(HistoryBufferedUpdates(history_buffered_updates));
    provide_context(HistoryBufferModeActive(history_buffer_mode_active));
    provide_context(NeedsLiveResync(needs_live_resync));
    provide_context(LiveResyncInFlight(live_resync_in_flight));
    provide_context(LiveHandoffResyncCount(live_handoff_resync_count));
    provide_context(SseSeqGapDetectedCount(sse_seq_gap_detected_count));
    provide_context(HistoryBufferSizeMax(history_buffer_size_max));
    provide_context(LiveSeasonScalarSample(live_season_scalar_sample));
    provide_context(HistorySeasonScalarSample(history_season_scalar_sample));
    provide_context(HistorySeasonLeaderboard(history_season_leaderboard));
    provide_context(TerritoryGeometryStore(territory_geometry));
    provide_context(GuildColorStore(guild_colors));
    provide_context(crate::tower::TowerState::new());
    provide_context(IsMobile(is_mobile));
    provide_context(PeekTerritory(peek_territory));
    provide_context(SelectedGuild(selected_guild));
    provide_context(DetailReturnGuild(detail_return_guild));
    provide_context(GuildOnlineData(guild_online_data));
    provide_context(ShowLeaderboardTerritoryCount(
        show_leaderboard_territory_count,
    ));
    provide_context(ShowLeaderboardOnline(show_leaderboard_online));
    provide_context(LeaderboardSortBySr(leaderboard_sort_by_sr));
    provide_context(HeatModeEnabled(heat_mode_enabled));
    provide_context(HeatLiveSourceSetting(heat_live_source));
    provide_context(HeatHistoryBasisSetting(heat_history_basis));
    provide_context(HeatSelectedSeasonId(heat_selected_season_id));
    provide_context(HeatEntriesByTerritory(heat_entries_by_territory));
    provide_context(HeatMaxTakeCount(heat_max_take_count));
    provide_context(HeatFallbackApplied(heat_fallback_applied));
    provide_context(HeatWindowLabel(heat_window_label));
    provide_context(HeatMetaState(heat_meta));

    Effect::new(move || {
        if reset_settings_trigger.get() == 0 {
            return;
        }

        let defaults = SettingsV2::default();
        show_connections.set(defaults.show_connections);
        abbreviate_names.set(defaults.abbreviate_names);
        show_countdown.set(defaults.show_countdown);
        show_granular_map_time.set(defaults.granular_map_time);
        show_compound_map_time.set(defaults.compound_map_time);
        show_names.set(defaults.show_names);
        thick_cooldown_borders.set(defaults.thick_cooldown_borders);
        bold_connections.set(defaults.bold_connections);
        connection_opacity_scale.set(clamp_connection_opacity_scale(
            defaults.connection_opacity_scale,
        ));
        connection_thickness_scale.set(clamp_connection_thickness_scale(
            defaults.connection_thickness_scale,
        ));
        resource_highlight.set(defaults.resource_highlight);
        show_resource_icons.set(defaults.show_resource_icons);
        show_territory_ornaments.set(defaults.show_territory_ornaments);
        manual_sr_scalar.set(season_scalar::clamp_manual_scalar(
            defaults.manual_sr_scalar,
        ));
        auto_sr_scalar_enabled.set(defaults.auto_sr_scalar_enabled);
        show_leaderboard_sr_gain.set(defaults.show_leaderboard_sr_gain);
        show_leaderboard_sr_value.set(defaults.show_leaderboard_sr_value);
        show_leaderboard_territory_count.set(defaults.show_leaderboard_territory_count);
        show_leaderboard_online.set(defaults.show_leaderboard_online);
        leaderboard_sort_by_sr.set(defaults.leaderboard_sort_by_sr);
        heat_mode_enabled.set(defaults.heat_mode_enabled);
        heat_live_source.set(defaults.heat_live_source);
        heat_history_basis.set(defaults.heat_history_basis);
        heat_selected_season_id.set(defaults.heat_selected_season_id);
        readable_font.set(defaults.readable_font);
        show_minimap.set(defaults.show_minimap);
        name_color.set(defaults.name_color);
        tag_color.set(defaults.tag_color);
        label_scale_master.set(clamp_label_scale_master(defaults.label_scale_master));
        label_scale_static.set(clamp_label_scale_group(defaults.label_scale_static));
        label_scale_static_name.set(clamp_label_scale_group(
            defaults
                .label_scale_static_name
                .unwrap_or(defaults.label_scale_static),
        ));
        label_scale_dynamic.set(clamp_label_scale_group(defaults.label_scale_dynamic));
        label_scale_icons.set(clamp_label_scale_group(defaults.label_scale_icons));
    });

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
    history::check_availability(history_available);

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
            let (source, at) = if mode_now == MapMode::History {
                let source = match heat_history_basis.get_untracked() {
                    HeatHistoryBasis::SeasonCumulative => HistoryHeatSource::Season,
                    HeatHistoryBasis::AllTimeCumulative => HistoryHeatSource::AllTime,
                };
                (source, at_override.or(history_timestamp.get_untracked()))
            } else {
                let source = match heat_live_source.get_untracked() {
                    HeatLiveSource::Season => HistoryHeatSource::Season,
                    HeatLiveSource::AllTime => HistoryHeatSource::AllTime,
                };
                (source, None)
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
                        heat_max_take_count.set(payload.max_take_count);
                        heat_fallback_applied.set(payload.fallback_applied);
                        heat_window_label.set(format_heat_window_label(&payload, mode_now));
                    }
                    Err(e) => {
                        if heat_refresh_nonce.get_untracked() != request_nonce {
                            return;
                        }
                        web_sys::console::warn_1(&format!("heat fetch failed: {e}").into());
                        heat_entries_by_territory.set(HashMap::new());
                        heat_max_take_count.set(0);
                        heat_fallback_applied.set(false);
                        heat_window_label.set(String::new());
                    }
                }
            });
        }
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
                heat_max_take_count.set(0);
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
                        let selected_before = heat_selected_season_id.get_untracked();
                        let selected_after =
                            normalize_heat_selected_season_id(&meta, selected_before);
                        heat_meta.set(Some(meta));
                        if selected_after != selected_before {
                            heat_selected_season_id.set(selected_after);
                        } else if selected_before.is_none() {
                            // No valid season id to set; refresh so season mode can fall back server-side.
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
            heat_live_source.track();
            heat_history_basis.track();
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

    // Poll shared server-side scalar estimate while in live mode.
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            if map_mode.get_untracked() == MapMode::Live && auto_sr_scalar_enabled.get_untracked() {
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
                        }
                    } else if let Some(n) = val.as_u64() {
                        // Legacy format: plain u32
                        GuildOnlineInfo {
                            online: n as u32,
                            season_rating: None,
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
                    let url = format!("/api/guilds/online?names={}", query);
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

    // Persist settings to localStorage on any change
    Effect::new(move || {
        let settings = SettingsV2 {
            show_connections: show_connections.get(),
            abbreviate_names: abbreviate_names.get(),
            show_countdown: show_countdown.get(),
            granular_map_time: show_granular_map_time.get(),
            compound_map_time: show_compound_map_time.get(),
            show_names: show_names.get(),
            thick_cooldown_borders: thick_cooldown_borders.get(),
            bold_connections: bold_connections.get(),
            connection_opacity_scale: clamp_connection_opacity_scale(
                connection_opacity_scale.get(),
            ),
            connection_thickness_scale: clamp_connection_thickness_scale(
                connection_thickness_scale.get(),
            ),
            sidebar_open: sidebar_open.get(),
            resource_highlight: resource_highlight.get(),
            show_resource_icons: show_resource_icons.get(),
            show_territory_ornaments: show_territory_ornaments.get(),
            manual_sr_scalar: season_scalar::clamp_manual_scalar(manual_sr_scalar.get()),
            auto_sr_scalar_enabled: auto_sr_scalar_enabled.get(),
            show_leaderboard_sr_gain: show_leaderboard_sr_gain.get(),
            show_leaderboard_sr_value: show_leaderboard_sr_value.get(),
            show_leaderboard_territory_count: show_leaderboard_territory_count.get(),
            show_leaderboard_online: show_leaderboard_online.get(),
            leaderboard_sort_by_sr: leaderboard_sort_by_sr.get(),
            heat_mode_enabled: heat_mode_enabled.get(),
            heat_live_source: heat_live_source.get(),
            heat_history_basis: heat_history_basis.get(),
            heat_selected_season_id: heat_selected_season_id.get(),
            readable_font: readable_font.get(),
            show_minimap: show_minimap.get(),
            name_color: name_color.get(),
            tag_color: tag_color.get(),
            label_scale_master: clamp_label_scale_master(label_scale_master.get()),
            label_scale_static: clamp_label_scale_group(label_scale_static.get()),
            label_scale_static_name: Some(clamp_label_scale_group(label_scale_static_name.get())),
            label_scale_dynamic: clamp_label_scale_group(label_scale_dynamic.get()),
            label_scale_icons: clamp_label_scale_group(label_scale_icons.get()),
            sidebar_width: clamp_sidebar_width(sidebar_width.get()),
        };
        let _ = gloo_storage::LocalStorage::set("sequoia_settings_v2", &settings);
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

    // Connect to SSE on mount
    Effect::new(move || {
        sse::connect(territories, connection);
        on_cleanup(|| {
            sse::disconnect();
        });
    });

    // Once initial territory data arrives, defer non-essential boot tasks slightly.
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
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                800,
            );
            cb.forget();
        } else {
            deferred_boot_ready.set(true);
        }
    });

    // Keep shell step text tied to real startup milestones.
    Effect::new(move || {
        let has_territories = !territories.get().is_empty();
        if deferred_boot_ready.get() {
            set_loading_shell_step("Starting renderer");
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
        if let Some(window) = web_sys::window() {
            let cb = wasm_bindgen::closure::Closure::once(|| {
                remove_loading_shell();
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                240,
            );
            cb.forget();
        } else {
            remove_loading_shell();
        }
    });

    // Schedule background tile loading only after first live paint and idle time.
    Effect::new(move || {
        if !deferred_boot_ready.get() || tile_fetch_scheduled.get_untracked() {
            return;
        }
        tile_fetch_scheduled.set(true);

        let Some(window) = web_sys::window() else {
            let (canvas_w, canvas_h) = canvas_dimensions();
            let context =
                tiles::TileFetchContext::new(viewport.get_untracked(), canvas_w, canvas_h);
            tiles::fetch_tiles(loaded_tiles, context);
            return;
        };

        let callback = wasm_bindgen::closure::Closure::once(move || {
            let (canvas_w, canvas_h) = canvas_dimensions();
            let context =
                tiles::TileFetchContext::new(viewport.get_untracked(), canvas_w, canvas_h);
            tiles::fetch_tiles(loaded_tiles, context);
        });
        let mut scheduled = false;
        if let Ok(idle_fn) =
            Reflect::get(window.as_ref(), &JsValue::from_str("requestIdleCallback"))
            && let Ok(idle_fn) = idle_fn.dyn_into::<Function>()
        {
            let _ = idle_fn.call1(window.as_ref(), callback.as_ref().unchecked_ref());
            scheduled = true;
        }
        if !scheduled {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                4_000,
            );
        }
        callback.forget();
    });

    // Lazy-load the icon atlas only when resource icons are enabled.
    Effect::new(move || {
        if !deferred_boot_ready.get() || !show_resource_icons.get() || icons_loaded.get_untracked()
        {
            return;
        }
        icons_loaded.set(true);
        icons::load_resource_atlas(loaded_icons);
    });

    // Start playback engine (runs continuously, only active when playing)
    Effect::new(move || {
        crate::playback::start_playback_engine();
    });

    // Global keyboard shortcuts
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
                let key = e.key();
                let target_tag = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    .map(|el| el.tag_name())
                    .unwrap_or_default();

                // Don't intercept when typing in an input
                if target_tag == "INPUT" || target_tag == "TEXTAREA" {
                    if key == "Escape"
                        && let Some(el) = e
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        el.blur().ok();
                    }
                    return;
                }

                match key.as_str() {
                    "Escape" => {
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
                        hovered.set(None);
                        if sidebar_transient.get_untracked() {
                            sidebar_open.set(false);
                            sidebar_transient.set(false);
                        }
                    }
                    "/" => {
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
                    "a" => {
                        abbreviate_names.update(|v| *v = !*v);
                    }
                    "n" => {
                        show_names.update(|v| *v = !*v);
                    }
                    "t" => {
                        show_countdown.update(|v| *v = !*v);
                    }
                    "c" => {
                        show_connections.update(|v| *v = !*v);
                    }
                    "b" => {
                        bold_connections.update(|v| *v = !*v);
                    }
                    "f" => {
                        readable_font.update(|v| *v = !*v);
                    }
                    "p" => {
                        resource_highlight.update(|v| *v = !*v);
                    }
                    "m" => {
                        show_minimap.update(|v| *v = !*v);
                    }
                    "h" => {
                        let mode = map_mode.get_untracked();
                        match mode {
                            MapMode::Live => {
                                if !history_available.get_untracked() {
                                    history::check_availability(history_available);
                                    return;
                                }
                                if history_available.get_untracked() {
                                    history::enter_history_mode(history::EnterHistoryModeInput {
                                        mode: map_mode,
                                        history_timestamp,
                                        history_bounds,
                                        history_fetch_nonce,
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
                    " " => {
                        if map_mode.get_untracked() == MapMode::History {
                            e.prevent_default();
                            playback_active.update(|v| *v = !*v);
                        }
                    }
                    "[" => {
                        if map_mode.get_untracked() == MapMode::History {
                            history::step_backward(history::HistoryStepContext {
                                history_timestamp,
                                playback_active,
                                fetch: history::HistoryFetchContext {
                                    mode: map_mode,
                                    history_fetch_nonce,
                                    history_scalar_sample: history_season_scalar_sample,
                                    history_sr_leaderboard: history_season_leaderboard,
                                    geo_store: territory_geometry,
                                    guild_color_store: guild_colors,
                                    territories,
                                },
                            });
                        }
                    }
                    "]" => {
                        if map_mode.get_untracked() == MapMode::History {
                            history::step_forward(history::HistoryStepContext {
                                history_timestamp,
                                playback_active,
                                fetch: history::HistoryFetchContext {
                                    mode: map_mode,
                                    history_fetch_nonce,
                                    history_scalar_sample: history_season_scalar_sample,
                                    history_sr_leaderboard: history_season_leaderboard,
                                    geo_store: territory_geometry,
                                    guild_color_store: guild_colors,
                                    territories,
                                },
                            });
                        }
                    }
                    "r" | "0" => {
                        viewport.update(|vp| {
                            let territories = territories.get_untracked();
                            if territories.is_empty() {
                                return;
                            }
                            let (mut min_x, mut min_y, mut max_x, mut max_y) =
                                (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                            for ct in territories.values() {
                                let loc = &ct.territory.location;
                                min_x = min_x.min(loc.left() as f64);
                                min_y = min_y.min(loc.top() as f64);
                                max_x = max_x.max(loc.right() as f64);
                                max_y = max_y.max(loc.bottom() as f64);
                            }
                            let (cw, ch) = canvas_dimensions();
                            vp.fit_bounds(min_x, min_y, max_x, max_y, cw, ch);
                        });
                    }
                    "j" | "ArrowDown" => {
                        e.prevent_default();
                        let items = sidebar_items.get_untracked();
                        if !items.is_empty() {
                            sidebar_index.update(|i| *i = (*i + 1).min(items.len() - 1));
                        }
                    }
                    "k" | "ArrowUp" => {
                        e.prevent_default();
                        let items = sidebar_items.get_untracked();
                        if !items.is_empty() {
                            sidebar_index.update(|i| *i = i.saturating_sub(1));
                        }
                    }
                    "Enter" => {
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
                    "ArrowLeft" => {
                        e.prevent_default();
                        viewport.update(|vp| vp.pan(50.0 / vp.scale, 0.0));
                    }
                    "ArrowRight" => {
                        e.prevent_default();
                        viewport.update(|vp| vp.pan(-50.0 / vp.scale, 0.0));
                    }
                    "+" | "=" => {
                        e.prevent_default();
                        let (cw, ch) = canvas_dimensions();
                        viewport.update(|vp| vp.zoom_at(-120.0, cw / 2.0, ch / 2.0));
                    }
                    "-" => {
                        e.prevent_default();
                        let (cw, ch) = canvas_dimensions();
                        viewport.update(|vp| vp.zoom_at(120.0, cw / 2.0, ch / 2.0));
                    }
                    _ => {}
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

    view! {
        <div style="width: 100%; height: 100%; position: relative;">
            <div style="width: 100%; height: 100%; position: relative; overflow: hidden; background: #0c0e17;">
                <MapCanvas />
                // Minimap backdrop frame (desktop only)
                <div
                    style:display=move || if is_mobile.get() || !show_minimap.get() { "none" } else { "block" }
                    style:bottom=move || if map_mode.get() == MapMode::History { "68px" } else { "16px" }
                    style="position: absolute; left: 16px; z-index: 6; width: 200px; height: 280px; pointer-events: none; border: 1px solid rgba(58,63,92,0.6); border-radius: 4px; box-shadow: 0 4px 20px rgba(0,0,0,0.5), 0 0 1px rgba(168,85,247,0.12), inset 0 0 0 1px rgba(255,255,255,0.03);"
                >
                    // "MAP" label
                    <div style="position: absolute; top: 6px; left: 8px; font-family: 'Silkscreen', monospace; font-size: 0.62rem; color: rgba(245,197,66,0.5); letter-spacing: 0.1em;">"MAP"</div>
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
                // Mobile HUD buttons — bottom-right stack
                <MobileHistoryToggle />
                // Mobile FAB — opens sidebar
                <button
                    class="mobile-sidebar-fab"
                    style:display=move || {
                        if is_mobile.get() && !sidebar_open.get() { "flex" } else { "none" }
                    }
                    style="position: absolute; bottom: 16px; right: 16px; z-index: 20; width: 48px; height: 48px; border-radius: 12px; background: #13161f; border: 1px solid #3a3f5c; align-items: center; justify-content: center; cursor: pointer; box-shadow: 0 4px 16px rgba(0,0,0,0.5), 0 0 1px rgba(168,85,247,0.15); color: #f5c542; font-size: 1.4rem; font-family: 'JetBrains Mono', monospace; touch-action: manipulation;"
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
                style:width=move || {
                    if is_mobile.get() {
                        "100%".to_string()
                    } else {
                        format!("{:.0}px", sidebar_width.get())
                    }
                }
                style:transform=move || {
                    if is_mobile.get() {
                        if sidebar_open.get() { "translateY(0)" } else { "translateY(100%)" }
                    } else if sidebar_open.get() {
                        "translateX(0)"
                    } else {
                        "translateX(100%)"
                    }
                }
                style:pointer-events=move || if sidebar_open.get() { "auto" } else { "none" }
            >
                <BottomSheetHandle />
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

/// Toggle button for showing/hiding the sidebar. Attached to the sidebar's left edge.
#[component]
fn SidebarToggle() -> impl IntoView {
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();

    view! {
        <button
            class="sidebar-toggle"
            title=move || if sidebar_open.get() { "Hide sidebar" } else { "Show sidebar" }
            style="position: absolute; top: 16px; left: -44px; z-index: 11; width: 32px; height: 32px; background: #13161f; border: 1px solid #282c3e; border-radius: 6px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s; color: #5a5860; font-family: 'JetBrains Mono', monospace; font-size: 1.1rem; line-height: 1;"
            on:click=move |_| {
                sidebar_open.update(|v| *v = !*v);
                sidebar_transient.set(false);
            }
            on:mouseenter=move |e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("border-color", "rgba(245,197,66,0.4)").ok();
                    el.style().set_property("color", "#f5c542").ok();
                    el.style().set_property("background", "#1a1d2a").ok();
                }
            }
            on:mouseleave=move |e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("border-color", "#282c3e").ok();
                    el.style().set_property("color", "#5a5860").ok();
                    el.style().set_property("background", "#13161f").ok();
                }
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

/// Swipe-to-dismiss drag handle for the mobile bottom sheet.
#[component]
fn BottomSheetHandle() -> impl IntoView {
    use std::rc::Rc;

    let SidebarOpen(sidebar_open) = expect_context();
    let IsMobile(is_mobile) = expect_context();

    let drag_start_y: Rc<std::cell::Cell<f64>> = Rc::new(std::cell::Cell::new(0.0));
    let dragging: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));

    let drag_start_y_down = drag_start_y.clone();
    let dragging_down = dragging.clone();
    let drag_start_y_move = drag_start_y.clone();
    let dragging_move = dragging.clone();
    let drag_start_y_up = drag_start_y.clone();
    let dragging_up = dragging.clone();

    view! {
        <div
            class="bottom-sheet-handle"
            on:pointerdown=move |e: web_sys::PointerEvent| {
                if !is_mobile.get_untracked() { return; }
                drag_start_y_down.set(e.client_y() as f64);
                dragging_down.set(true);
                if let Some(target) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    target.set_pointer_capture(e.pointer_id()).ok();
                }
            }
            on:pointermove=move |e: web_sys::PointerEvent| {
                if !dragging_move.get() { return; }
                let delta = (e.client_y() as f64 - drag_start_y_move.get()).max(0.0);
                // Apply translate directly to sidebar wrapper parent
                if let Some(target) = e.target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    .and_then(|el| el.parent_element())
                    .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    target.style().set_property("transition", "none").ok();
                    target.style().set_property("transform", &format!("translateY({}px)", delta)).ok();
                }
            }
            on:pointerup=move |e: web_sys::PointerEvent| {
                if !dragging_up.get() { return; }
                dragging_up.set(false);
                let delta = (e.client_y() as f64 - drag_start_y_up.get()).max(0.0);
                if let Some(target) = e.target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    .and_then(|el| el.parent_element())
                    .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    // Restore CSS transition
                    target.style().remove_property("transition").ok();
                    target.style().remove_property("transform").ok();
                }
                if delta > 80.0 {
                    sidebar_open.set(false);
                }
            }
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
    let HistoryTimestamp(history_timestamp) = expect_context();
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
            style:display=move || {
                if is_mobile.get() && !sidebar_open.get() && history_available.get() && !is_history() {
                    "flex"
                } else {
                    "none"
                }
            }
            style:bottom=move || if is_history() { "76px" } else { "72px" }
            style:background=move || if is_history() { "#f5c542" } else { "#13161f" }
            style:color=move || if is_history() { "#13161f" } else { "#9a9590" }
            style:border-color=move || if is_history() { "#f5c542" } else { "#3a3f5c" }
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
                        mode: map_mode,
                        history_timestamp,
                        history_bounds,
                        history_fetch_nonce,
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
    base_resources: Resources,
    resources: Resources,
    resources_from_live: bool,
    provenance_source: Option<String>,
    live_production_rates: Option<Resources>,
    live_storage_capacity: Option<Resources>,
    takes_in_window: Option<u64>,
}

/// Tooltip that follows the mouse cursor when hovering a territory.
#[component]
fn Tooltip() -> impl IntoView {
    let Hovered(hovered) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let mouse_pos: RwSignal<(f64, f64)> = expect_context();
    let tick: RwSignal<i64> = expect_context();
    let CurrentMode(mode) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();

    let tooltip_info = Memo::new(move |_| {
        let reference_secs = if mode.get() == MapMode::History {
            history_timestamp.get().unwrap_or_else(|| tick.get())
        } else {
            tick.get()
        };
        let name = hovered.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let base_resources = ct.territory.resources.clone();
        let live_held_resources = ct
            .territory
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.held_resources.clone())
            .filter(|value| !value.is_empty());
        let live_production_rates = ct
            .territory
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.production_rates.clone())
            .filter(|value| !value.is_empty());
        let live_storage_capacity = ct
            .territory
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.storage_capacity.clone())
            .filter(|value| !value.is_empty());
        let resources = live_held_resources
            .clone()
            .unwrap_or_else(|| base_resources.clone());
        let resources_from_live = live_held_resources.is_some();
        let provenance_source = ct
            .territory
            .runtime
            .as_ref()
            .and_then(|rt| rt.provenance.as_ref())
            .map(|p| p.source.clone())
            .filter(|s| !s.trim().is_empty());
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
        let cooldown = if secs < 600 {
            let remaining = 600 - secs;
            let frac = remaining as f64 / 600.0;
            Some((format!("{}:{:02}", remaining / 60, remaining % 60), frac))
        } else {
            None
        };

        Some(TooltipInfo {
            name,
            guild_name: ct.territory.guild.name.clone(),
            guild_prefix: ct.territory.guild.prefix.clone(),
            held: format_hms(secs),
            guild_color: ct.guild_color,
            cooldown,
            treasury: TreasuryLevel::from_held_seconds(secs),
            base_resources,
            resources,
            resources_from_live,
            provenance_source,
            live_production_rates,
            live_storage_capacity,
            takes_in_window,
        })
    });

    view! {
        {move || {
            let Some(info) = tooltip_info.get() else {
                return view! { <div style="display:none;" /> }.into_any();
            };
            let (x, y) = mouse_pos.get();
            let (r, g, b) = info.guild_color;
            let (tr, tg, tb) = info.treasury.color_rgb();
            let buff = info.treasury.buff_percent();
            let treasury_label = info.treasury.label();
            let source_badge = if info.resources_from_live { "LIVE" } else { "MAP" };
            let source_badge_style = if info.resources_from_live {
                "background: rgba(var(--accent-live-rgb),0.14); border: 1px solid rgba(var(--accent-live-rgb),0.32); color: var(--accent-live);"
            } else {
                "background: rgba(154,149,144,0.12); border: 1px solid rgba(154,149,144,0.24); color: #9a9590;"
            };
            let is_iris = info
                .provenance_source
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("iris"));

            let render_resource_section = |title: &str,
                                           title_color: &str,
                                           chip_border: &str,
                                           _value_color: &str,
                                           resources: &Resources,
                                           double_source: &Resources| {
                let chips = tooltip_resource_items(resources, double_source)
                    .into_iter()
                    .filter(|(val, _, _, _)| *val > 0)
                    .collect::<Vec<_>>();
                if chips.is_empty() {
                    return view! { <div style="display:none;" /> }.into_any();
                }
                view! {
                    <div style="padding: 7px 0 3px; border-top: 1px solid rgba(40,44,62,0.6);">
                        <div style={format!(
                            "font-family: 'Silkscreen', monospace; font-size: 0.64rem; text-transform: uppercase; letter-spacing: 0.10em; color: {title_color}; margin-bottom: 5px;"
                        )}>
                            {title.to_string()}
                        </div>
                        <div style="display: flex; flex-wrap: wrap; gap: 4px;">
                            {chips.into_iter().map(|(val, is_double, icon, label)| {
                                let icon_style = icons::sprite_style(icon, 12).unwrap_or_default();
                                let double_style = icon_style.clone();
                                let amount = format_resource_compact(val);
                                view! {
                                    <div style={format!(
                                        "display: flex; align-items: center; gap: 4px; background: #1a1d2a; padding: 3px 7px; border-radius: 4px; border: 1px solid {chip_border};"
                                    )}>
                                        <span style={icon_style} />
                                        {(is_double).then(|| view! { <span style={double_style} /> })}
                                        <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.70rem; color: #e2e0d8; font-variant-numeric: tabular-nums;">
                                            {amount}
                                        </span>
                                        <span style={format!(
                                            "font-family: 'Inter', system-ui, sans-serif; font-size: 0.60rem; color: {title_color};"
                                        )}>
                                            {label}
                                        </span>
                                    </div>
                                }
                            }).collect::<Vec<_>>()}
                        </div>
                    </div>
                }.into_any()
            };

            let held_section = if info.resources_from_live {
                render_resource_section(
                    "Held Resources",
                    "var(--accent-live)",
                    "rgba(var(--accent-live-rgb),0.25)",
                    "#dbe8ff",
                    &info.resources,
                    &info.base_resources,
                )
            } else {
                render_resource_section(
                    "Resources (Map)",
                    "#9a9590",
                    "rgba(154,149,144,0.25)",
                    "#e2e0d8",
                    &info.resources,
                    &info.base_resources,
                )
            };
            let production_section = info
                .live_production_rates
                .as_ref()
                .map(|resources| {
                    render_resource_section(
                        "Production/Hr (Live)",
                        "var(--accent-live)",
                        "rgba(var(--accent-live-rgb),0.25)",
                        "#d4e9ff",
                        resources,
                        &info.base_resources,
                    )
                })
                .unwrap_or_else(|| view! { <div style="display:none;" /> }.into_any());
            let storage_section = info
                .live_storage_capacity
                .as_ref()
                .map(|resources| {
                    render_resource_section(
                        "Storage Capacity (Live)",
                        "var(--accent-live)",
                        "rgba(var(--accent-live-rgb),0.25)",
                        "#dcf1ff",
                        resources,
                        &info.base_resources,
                    )
                })
                .unwrap_or_else(|| view! { <div style="display:none;" /> }.into_any());

            view! {
                <div
                    class="tooltip-animate"
                    style:left=format!("{}px", x + 16.0)
                    style:top=format!("{}px", y - 8.0)
                    style="position: fixed; pointer-events: none; z-index: 100; min-width: 280px; max-width: 340px; background: #13161f; border: 1px solid #282c3e; border-radius: 8px; overflow: hidden; box-shadow: 0 8px 24px rgba(0,0,0,0.5), 0 0 0 1px rgba(255,255,255,0.02) inset; backdrop-filter: blur(2px); display: flex; flex-direction: row;"
                >
                    <div style={format!(
                        "width: 4px; flex-shrink: 0; background: linear-gradient(180deg, {} 0%, {} 100%);",
                        rgba_css(r, g, b, 0.85),
                        rgba_css(r, g, b, 0.30),
                    )} />
                    <div style="padding: 12px 14px 10px; flex: 1; min-width: 0;">
                        // Header: swatch + territory name + source badge
                        <div style="display: flex; align-items: center; gap: 8px; margin-bottom: 3px;">
                            <div style={format!(
                                "width: 14px; height: 14px; border-radius: 3px; border: 1px solid rgba(255,255,255,0.10); background: {}; flex-shrink: 0; box-shadow: 0 0 4px {};",
                                rgba_css(r, g, b, 0.75),
                                rgba_css(r, g, b, 0.25),
                            )} />
                            <div style="font-size: 0.92rem; font-weight: 700; color: #e2e0d8; font-family: 'Silkscreen', monospace; line-height: 1.2; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0;">
                                {info.name.clone()}
                            </div>
                            <span style="display: inline-flex; align-items: center; gap: 4px; flex-shrink: 0;">
                                {is_iris.then(|| view! {
                                    <a
                                        href=crate::IRIS_RELEASES_URL
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        title="Open Iris releases"
                                        style="display: inline-flex; pointer-events: auto; text-decoration: none;"
                                    >
                                        <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.60rem; letter-spacing: 0.08em; padding: 2px 7px; border-radius: 999px; text-transform: uppercase; background: rgba(168,85,247,0.14); border: 1px solid rgba(168,85,247,0.32); color: #a855f7; cursor: pointer;">
                                            "IRIS"
                                        </span>
                                    </a>
                                })}
                                <span style={format!(
                                    "font-family: 'JetBrains Mono', monospace; font-size: 0.60rem; letter-spacing: 0.08em; padding: 2px 7px; border-radius: 999px; text-transform: uppercase; {}",
                                    source_badge_style
                                )}>
                                    {source_badge}
                                </span>
                            </span>
                        </div>
                        // Guild name + prefix
                        <div style="display: flex; align-items: baseline; gap: 4px; margin-left: 22px; margin-bottom: 8px;">
                            <a
                                href=crate::guild_stats_url(&info.guild_name)
                                target="_blank"
                                rel="noopener noreferrer"
                                title="Open Wynncraft guild stats"
                                style="font-size: 0.78rem; color: #f5c542; font-family: 'Inter', system-ui, sans-serif; text-decoration: none; pointer-events: auto;"
                            >
                                {info.guild_name.clone()}
                            </a>
                            <span style="font-size: 0.70rem; color: #9a9590; font-family: 'JetBrains Mono', monospace;">"[" {info.guild_prefix.clone()} "]"</span>
                        </div>

                        // Key-value rows
                        <div style="display: flex; flex-direction: column;">
                            <div style="display: flex; justify-content: space-between; align-items: center; padding: 6px 0; border-top: 1px solid rgba(40,44,62,0.6);">
                                <span style="color: #9a9590; font-size: 0.72rem; font-family: 'Inter', system-ui, sans-serif;">"Held"</span>
                                <span style="color: #e2e0d8; font-size: 0.82rem; font-family: 'JetBrains Mono', monospace; font-variant-numeric: tabular-nums;">{info.held.clone()}</span>
                            </div>
                            {match info.cooldown.clone() {
                                Some((remaining, frac)) => view! {
                                    <div style="padding: 6px 0; border-top: 1px solid rgba(40,44,62,0.6);">
                                        <div style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 5px;">
                                            <span style="color: #f5c542; font-size: 0.72rem; font-family: 'Silkscreen', monospace;">"Cooldown"</span>
                                            <span style="color: #f5c542; font-size: 0.82rem; font-family: 'JetBrains Mono', monospace; font-variant-numeric: tabular-nums;">{remaining}</span>
                                        </div>
                                        <div style="height: 4px; background: rgba(255,255,255,0.06); border-radius: 2px; overflow: hidden;">
                                            <div style={format!(
                                                "height: 100%; width: {:.1}%; background: linear-gradient(to right, #f5c542, #d4a030); border-radius: 2px; box-shadow: 0 0 6px rgba(245,197,66,0.1);",
                                                frac * 100.0
                                            )} />
                                        </div>
                                    </div>
                                }.into_any(),
                                None => view! {
                                    <div style="display: flex; justify-content: space-between; align-items: center; padding: 6px 0; border-top: 1px solid rgba(40,44,62,0.6);">
                                        <span style="color: #9a9590; font-size: 0.72rem; font-family: 'Inter', system-ui, sans-serif;">"Cooldown"</span>
                                        <span style="color: #50c878; font-size: 0.82rem; font-family: 'JetBrains Mono', monospace;">"Ready"</span>
                                    </div>
                                }.into_any(),
                            }}
                            <div style="display: flex; justify-content: space-between; align-items: center; padding: 6px 0; border-top: 1px solid rgba(40,44,62,0.6);">
                                <span style="color: #9a9590; font-size: 0.72rem; font-family: 'Inter', system-ui, sans-serif;">"Treasury"</span>
                                <span style="display: inline-flex; align-items: center; gap: 5px;">
                                    <span style={format!("color: {}; font-family: 'JetBrains Mono', monospace; font-size: 0.78rem;", rgba_css(tr, tg, tb, 1.0))}>{treasury_label}</span>
                                    {(buff > 0).then(|| view! {
                                        <span style={format!(
                                            "font-size: 0.65rem; font-family: 'JetBrains Mono', monospace; color: {}; background: {}; padding: 1px 5px; border-radius: 3px;",
                                            rgba_css(tr, tg, tb, 0.9),
                                            rgba_css(tr, tg, tb, 0.08),
                                        )}>{format!("+{}%", buff)}</span>
                                    })}
                                </span>
                            </div>
                            {info.takes_in_window.map(|count| view! {
                                <div style="display: flex; justify-content: space-between; align-items: center; padding: 6px 0; border-top: 1px solid rgba(40,44,62,0.6);">
                                    <span style="color: #9a9590; font-size: 0.72rem; font-family: 'Inter', system-ui, sans-serif;">"Takes in window"</span>
                                    <span style="color: #e2e0d8; font-size: 0.78rem; font-family: 'JetBrains Mono', monospace; font-variant-numeric: tabular-nums;">{count}</span>
                                </div>
                            })}
                        </div>

                        {held_section}
                        {production_section}
                        {storage_section}
                    </div>
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
    let HistoryTimestamp(history_timestamp) = expect_context();
    let Selected(selected) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();

    let peek_info = Memo::new(move |_| {
        let reference_secs = if mode.get() == MapMode::History {
            history_timestamp.get().unwrap_or_else(|| tick.get())
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
        let acquired = ct.territory.acquired.to_rfc3339();
        let secs = chrono::DateTime::parse_from_rfc3339(&acquired)
            .map(|dt| (reference_secs - dt.timestamp()).max(0))
            .unwrap_or(0);
        let held = format_hms(secs);
        let treasury = TreasuryLevel::from_held_seconds(secs);
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
            let Some(info) = peek_info.get() else {
                return view! { <div style="display:none;" /> }.into_any();
            };
            let (r, g, b) = info.4;
            let treasury = info.5;
            let takes_in_window = info.6;
            let (tr, tg, tb) = treasury.color_rgb();
            let name = info.0.clone();
            let is_history = mode.get_untracked() == MapMode::History;
            let bottom_px = if is_history { 96 } else { 16 };
            view! {
                <div
                    class="peek-card-animate"
                    style:bottom=format!("{}px", bottom_px)
                    style="position: fixed; left: 16px; right: 16px; z-index: 90; background: #161921; border: 1px solid #282c3e; border-radius: 10px; overflow: hidden; box-shadow: 0 4px 20px rgba(0,0,0,0.6); display: flex; flex-direction: row;"
                >
                    <div style={format!("width: 4px; flex-shrink: 0; background: {};", rgba_css(r, g, b, 0.85))} />
                    <div style="padding: 12px 14px; flex: 1; display: flex; flex-direction: column; gap: 4px;">
                        <div style="font-size: 0.85rem; font-weight: 700; color: #e2e0d8; font-family: 'Silkscreen', monospace; line-height: 1.3;">
                            <span style="color: #9a9590; font-weight: 400;">"[" {info.2.clone()} "] "</span>
                            {info.1}
                        </div>
                        <div style="font-size: 0.72rem; color: #9a9590; font-family: 'JetBrains Mono', monospace;">
                            {info.0.clone()}
                        </div>
                        <div style="font-size: 0.68rem; display: flex; justify-content: space-between; align-items: center; gap: 8px; margin-top: 2px;">
                            <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Held"</span>
                            <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-variant-numeric: tabular-nums;">{info.3}</span>
                        </div>
                        {takes_in_window.map(|count| view! {
                            <div style="font-size: 0.68rem; display: flex; justify-content: space-between; align-items: center; gap: 8px;">
                                <span style="color: #9a9590; font-family: 'Inter', system-ui, sans-serif;">"Takes in window"</span>
                                <span style="color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-variant-numeric: tabular-nums;">{count}</span>
                            </div>
                        })}
                        <div style="font-size: 0.68rem; font-family: 'JetBrains Mono', monospace; display: flex; align-items: center; gap: 4px;">
                            <span style={format!("color: {}; font-size: 0.52rem;", rgba_css(tr, tg, tb, 1.0))}>{"\u{25C6}"}</span>
                            <span style={format!("color: {};", rgba_css(tr, tg, tb, 0.9))}>{treasury.label()}</span>
                        </div>
                    </div>
                    <button
                        style="align-self: center; margin-right: 14px; min-height: 44px; min-width: 44px; padding: 8px 16px; background: #1a1d2a; border: 1px solid #3a3f5c; border-radius: 6px; color: #f5c542; font-family: 'JetBrains Mono', monospace; font-size: 0.72rem; cursor: pointer; touch-action: manipulation; white-space: nowrap;"
                        on:click=move |_| {
                            detail_return_guild.set(None);
                            selected.set(Some(name.clone()));
                            sidebar_open.set(true);
                            peek_territory.set(None);
                        }
                    >
                        "Details \u{203A}"
                    </button>
                </div>
            }.into_any()
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_SIDEBAR_WIDTH, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SettingsV2,
        clamp_sidebar_width, normalize_heat_selected_season_id,
    };
    use sequoia_shared::history::{HistoryHeatMeta, HistoryHeatSeasonWindow};

    #[test]
    fn clamp_sidebar_width_enforces_limits() {
        assert_eq!(clamp_sidebar_width(240.0), SIDEBAR_WIDTH_MIN);
        assert_eq!(clamp_sidebar_width(420.0), 420.0);
        assert_eq!(clamp_sidebar_width(900.0), SIDEBAR_WIDTH_MAX);
    }

    #[test]
    fn settings_v2_deserialization_defaults_sidebar_width() {
        let parsed: SettingsV2 = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(parsed.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
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

        assert_eq!(
            normalize_heat_selected_season_id(&meta, Some(999)),
            Some(31)
        );
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
}
