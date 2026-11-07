//! The viewer's settings as plain data: what each one is, its default and its range.

use serde::{Deserialize, Serialize};

pub(crate) use sequoia_map_engine::settings::{
    ConnectionStyle, CooldownDisplay, HqCrownStyle, NameColor, WarOutline, WarOutlineColor,
    WarOutlineStyle,
};

/// Every setting the menu shows, saved under these names in the settings blob. A blob missing
/// a setting, saved before it existed, takes its default.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub show_connections: bool,
    pub abbreviate_names: bool,
    pub show_countdown: bool,
    pub granular_map_time: bool,
    pub compound_map_time: bool,
    pub show_names: bool,
    pub show_claim_labels: bool,
    pub show_far_zoom_territory_tags: bool,
    pub thick_cooldown_borders: bool,
    pub bold_connections: bool,
    pub connection_style: ConnectionStyle,
    pub connection_opacity_scale: f64,
    pub connection_solid_opacity: f64,
    pub connection_thickness_scale: f64,
    /// The war panel; the key predates the panel's other contents.
    pub show_war_queue: bool,
    pub show_war_markers: bool,
    pub activity_include_unconfirmed: bool,
    pub show_player_heads: bool,
    pub player_head_render_head: bool,
    pub player_head_render_label: bool,
    pub player_head_size: f64,
    #[serde(flatten)]
    pub overlay: Overlay,
    pub resource_highlight_opacity: f64,
    pub territory_fill_opacity: f64,
    pub show_resource_icons: bool,
    pub show_territory_ornaments: bool,
    #[serde(deserialize_with = "lenient_default")]
    pub hq_crown_style: HqCrownStyle,
    #[serde(deserialize_with = "lenient_default")]
    pub cooldown_display: CooldownDisplay,
    pub hq_crown_scale: f64,
    #[serde(deserialize_with = "lenient_war_outline")]
    pub war_outline: WarOutline,
    pub manual_sr_scalar: f64,
    pub auto_sr_scalar_enabled: bool,
    pub show_leaderboard_sr_gain: bool,
    pub show_leaderboard_sr_value: bool,
    pub show_leaderboard_territory_count: bool,
    pub show_leaderboard_online: bool,
    pub heat_mode_enabled: bool,
    pub heat_live_source: HeatLiveSource,
    pub heat_history_basis: HeatHistoryBasis,
    /// `None` is Latest, which the server reads as the newest season and so follows a new one.
    pub heat_selected_season_id: Option<i32>,
    pub readable_font: bool,
    pub show_minimap: bool,
    pub name_color: NameColor,
    pub tag_color: NameColor,
    pub label_scale_master: f64,
    pub label_scale_static: f64,
    pub label_scale_static_name: f64,
    pub label_scale_dynamic: f64,
    pub label_scale_icons: f64,
    pub show_debug_info: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_connections: true,
            abbreviate_names: true,
            show_countdown: false,
            granular_map_time: false,
            compound_map_time: true,
            show_names: false,
            show_claim_labels: false,
            show_far_zoom_territory_tags: true,
            thick_cooldown_borders: true,
            bold_connections: false,
            connection_style: ConnectionStyle::Classic,
            connection_opacity_scale: DEFAULT_CONNECTION_OPACITY_SCALE,
            connection_solid_opacity: DEFAULT_CONNECTION_SOLID_OPACITY,
            connection_thickness_scale: DEFAULT_CONNECTION_THICKNESS_SCALE,
            show_war_queue: true,
            show_war_markers: true,
            activity_include_unconfirmed: false,
            show_player_heads: true,
            player_head_render_head: true,
            player_head_render_label: false,
            player_head_size: DEFAULT_PLAYER_HEAD_SIZE,
            overlay: Overlay::Off,
            resource_highlight_opacity: DEFAULT_RESOURCE_HIGHLIGHT_OPACITY,
            territory_fill_opacity: DEFAULT_TERRITORY_OPACITY,
            show_resource_icons: true,
            show_territory_ornaments: false,
            hq_crown_style: HqCrownStyle::default(),
            cooldown_display: CooldownDisplay::default(),
            hq_crown_scale: DEFAULT_HQ_CROWN_SCALE,
            war_outline: WarOutline::default(),
            manual_sr_scalar: DEFAULT_MANUAL_SR_SCALAR,
            auto_sr_scalar_enabled: true,
            show_leaderboard_sr_gain: false,
            show_leaderboard_sr_value: false,
            show_leaderboard_territory_count: true,
            show_leaderboard_online: true,
            heat_mode_enabled: false,
            heat_live_source: HeatLiveSource::AllTime,
            heat_history_basis: HeatHistoryBasis::SeasonCumulative,
            heat_selected_season_id: None,
            readable_font: false,
            show_minimap: false,
            name_color: NameColor::Guild,
            tag_color: NameColor::Guild,
            label_scale_master: DEFAULT_LABEL_SCALE,
            label_scale_static: DEFAULT_LABEL_SCALE,
            label_scale_static_name: DEFAULT_LABEL_SCALE,
            label_scale_dynamic: DEFAULT_LABEL_SCALE,
            label_scale_icons: DEFAULT_LABEL_SCALE,
            show_debug_info: false,
        }
    }
}

impl Settings {
    /// Every value within its control's range, as a hand-edited or older blob may not be.
    pub(crate) fn clamped(self) -> Self {
        Self {
            connection_opacity_scale: clamp_connection_opacity_scale(self.connection_opacity_scale),
            connection_solid_opacity: clamp_connection_solid_opacity(self.connection_solid_opacity),
            connection_thickness_scale: clamp_connection_thickness_scale(
                self.connection_thickness_scale,
            ),
            player_head_size: clamp_player_head_size(self.player_head_size),
            resource_highlight_opacity: clamp_resource_highlight_opacity(
                self.resource_highlight_opacity,
            ),
            territory_fill_opacity: clamp_territory_opacity(self.territory_fill_opacity),
            hq_crown_scale: clamp_hq_crown_scale(self.hq_crown_scale),
            war_outline: self.war_outline.clamped(),
            manual_sr_scalar: crate::season_scalar::clamp_manual_scalar(self.manual_sr_scalar),
            label_scale_master: clamp_label_scale_master(self.label_scale_master),
            label_scale_static: clamp_label_scale_group(self.label_scale_static),
            label_scale_static_name: clamp_label_scale_group(self.label_scale_static_name),
            label_scale_dynamic: clamp_label_scale_group(self.label_scale_dynamic),
            label_scale_icons: clamp_label_scale_group(self.label_scale_icons),
            ..self
        }
    }
}

/// The one territory overlay that can be on at a time, saved as one flag each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "OverlayFlags", into = "OverlayFlags")]
pub(crate) enum Overlay {
    #[default]
    Off,
    Resources,
    Defense,
    Intel,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
struct OverlayFlags {
    resource_highlight: bool,
    defense_highlight: bool,
    map_intel_enabled: bool,
}

/// Resources wins if a blob ever held two at once, as the app always resolved it.
impl From<OverlayFlags> for Overlay {
    fn from(flags: OverlayFlags) -> Self {
        if flags.resource_highlight {
            Overlay::Resources
        } else if flags.defense_highlight {
            Overlay::Defense
        } else if flags.map_intel_enabled {
            Overlay::Intel
        } else {
            Overlay::Off
        }
    }
}

impl From<Overlay> for OverlayFlags {
    fn from(overlay: Overlay) -> Self {
        Self {
            resource_highlight: overlay == Overlay::Resources,
            defense_highlight: overlay == Overlay::Defense,
            map_intel_enabled: overlay == Overlay::Intel,
        }
    }
}

impl Overlay {
    /// The overlay a shortcut toggles: on, or off if it already is.
    pub(crate) fn toggled(self, overlay: Overlay) -> Self {
        if self == overlay {
            Overlay::Off
        } else {
            overlay
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeatLiveSource {
    Season,
    AllTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeatHistoryBasis {
    SeasonCumulative,
    AllTimeCumulative,
}

/// A style this build does not know, saved by a newer one, falls back to the default
/// rather than failing the whole settings blob.
fn lenient_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Like [`lenient_default`]: an outline saved by a newer build falls back to the
/// default rather than failing the whole settings blob.
fn lenient_war_outline<'de, D>(deserializer: D) -> Result<WarOutline, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value::<WarOutline>(value)
        .unwrap_or_default()
        .clamped())
}

pub(crate) const DEFAULT_MANUAL_SR_SCALAR: f64 = 1.5;

pub(crate) const DEFAULT_HQ_CROWN_SCALE: f64 = 1.0;
// Exact decimal twins of the renderer's `f32` bounds, so the slider snaps to its steps.
pub(crate) const HQ_CROWN_SCALE_MIN: f64 = 0.6;
pub(crate) const HQ_CROWN_SCALE_MAX: f64 = 1.6;

pub(crate) fn clamp_hq_crown_scale(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(HQ_CROWN_SCALE_MIN, HQ_CROWN_SCALE_MAX)
    } else {
        DEFAULT_HQ_CROWN_SCALE
    }
}

pub(crate) const DEFAULT_LABEL_SCALE: f64 = 1.0;
pub(crate) const DEFAULT_CONNECTION_OPACITY_SCALE: f64 = 1.0;
pub(crate) const DEFAULT_CONNECTION_THICKNESS_SCALE: f64 = 1.0;
pub(crate) const CONNECTION_OPACITY_SCALE_MIN: f64 = 0.60;
pub(crate) const CONNECTION_OPACITY_SCALE_MAX: f64 = 2.50;
pub(crate) const DEFAULT_CONNECTION_SOLID_OPACITY: f64 = 1.0;
pub(crate) const CONNECTION_SOLID_OPACITY_MIN: f64 = 0.20;
pub(crate) const CONNECTION_SOLID_OPACITY_MAX: f64 = 1.0;
pub(crate) const CONNECTION_THICKNESS_SCALE_MIN: f64 = 0.70;
pub(crate) const CONNECTION_THICKNESS_SCALE_MAX: f64 = 2.50;
// Exact decimal twins of the renderer's `f32` bounds, so the slider snaps to its steps.
/// Initial and reset opacity; also fills in saved settings that predate the slider.
pub(crate) const DEFAULT_RESOURCE_HIGHLIGHT_OPACITY: f64 = 0.45;
/// Faintest highlight the slider allows; resources still read against the map.
pub(crate) const RESOURCE_HIGHLIGHT_OPACITY_MIN: f64 = 0.15;
/// Firmest highlight the slider allows, short of fully hiding the map tiles.
pub(crate) const RESOURCE_HIGHLIGHT_OPACITY_MAX: f64 = 0.90;
pub(crate) const LABEL_SCALE_MASTER_MIN: f64 = 1.0;
pub(crate) const LABEL_SCALE_MASTER_MAX: f64 = 2.25;
pub(crate) const LABEL_SCALE_GROUP_MIN: f64 = 0.60;
pub(crate) const LABEL_SCALE_GROUP_MAX: f64 = 1.80;
/// On-screen edge length of a teammate head, in CSS pixels. Screen-space rather than
/// world-space on purpose: the head stays legible at every zoom, the way the map intel
/// markers do, instead of shrinking to nothing when you zoom out to the whole province.
pub(crate) const PLAYER_HEAD_SIZE_MIN: f64 = 8.0;
pub(crate) const PLAYER_HEAD_SIZE_MAX: f64 = 48.0;
pub(crate) const DEFAULT_PLAYER_HEAD_SIZE: f64 = 20.0;

pub(crate) fn clamp_connection_opacity_scale(value: f64) -> f64 {
    if value.is_nan() {
        return DEFAULT_CONNECTION_OPACITY_SCALE;
    }
    value.clamp(CONNECTION_OPACITY_SCALE_MIN, CONNECTION_OPACITY_SCALE_MAX)
}

pub(crate) fn clamp_connection_solid_opacity(value: f64) -> f64 {
    if value.is_nan() {
        return DEFAULT_CONNECTION_SOLID_OPACITY;
    }
    value.clamp(CONNECTION_SOLID_OPACITY_MIN, CONNECTION_SOLID_OPACITY_MAX)
}

/// The resting guild fill's opacity, the overlay as it has always been drawn; also fills in
/// saved settings that predate it.
pub(crate) const DEFAULT_TERRITORY_OPACITY: f64 = 0.26;
/// At 0 the guild fill is gone and borders keep a floor (see the renderer).
pub(crate) const TERRITORY_OPACITY_MIN: f64 = 0.0;
/// Fully opaque.
pub(crate) const TERRITORY_OPACITY_MAX: f64 = 1.0;

/// Guards `NaN` like the head size: a hand-edited blob must not hide the highlight.
pub(crate) fn clamp_territory_opacity(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(TERRITORY_OPACITY_MIN, TERRITORY_OPACITY_MAX)
    } else {
        DEFAULT_TERRITORY_OPACITY
    }
}

pub(crate) fn clamp_resource_highlight_opacity(value: f64) -> f64 {
    if value.is_nan() {
        return DEFAULT_RESOURCE_HIGHLIGHT_OPACITY;
    }
    value.clamp(
        RESOURCE_HIGHLIGHT_OPACITY_MIN,
        RESOURCE_HIGHLIGHT_OPACITY_MAX,
    )
}

pub(crate) fn clamp_connection_thickness_scale(value: f64) -> f64 {
    if value.is_nan() {
        return DEFAULT_CONNECTION_THICKNESS_SCALE;
    }
    value.clamp(
        CONNECTION_THICKNESS_SCALE_MIN,
        CONNECTION_THICKNESS_SCALE_MAX,
    )
}

/// Unlike the other clamps this one guards `NaN`: the value reaches us from a persisted
/// JSON blob a user can hand-edit, and a `NaN` size would silently draw nothing at all
/// rather than being pinned to a bound.
pub(crate) fn clamp_player_head_size(value: f64) -> f64 {
    if value.is_nan() {
        return DEFAULT_PLAYER_HEAD_SIZE;
    }
    value.clamp(PLAYER_HEAD_SIZE_MIN, PLAYER_HEAD_SIZE_MAX)
}

pub(crate) fn clamp_label_scale_master(value: f64) -> f64 {
    value.clamp(LABEL_SCALE_MASTER_MIN, LABEL_SCALE_MASTER_MAX)
}

pub(crate) fn clamp_label_scale_group(value: f64) -> f64 {
    value.clamp(LABEL_SCALE_GROUP_MIN, LABEL_SCALE_GROUP_MAX)
}

// Exact decimal twins of the renderer's `f32` outline bounds, so the sliders snap to their
// steps.
pub(crate) const WAR_OUTLINE_WIDTH_MIN: f64 = 0.5;
pub(crate) const WAR_OUTLINE_WIDTH_MAX: f64 = 2.0;
pub(crate) const WAR_OUTLINE_OPACITY_MIN: f64 = 0.3;
pub(crate) const WAR_OUTLINE_OPACITY_MAX: f64 = 1.0;

pub(crate) fn clamp_war_outline_width(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(WAR_OUTLINE_WIDTH_MIN, WAR_OUTLINE_WIDTH_MAX)
    } else {
        f64::from(WarOutline::default().width)
    }
}

pub(crate) fn clamp_war_outline_opacity(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(WAR_OUTLINE_OPACITY_MIN, WAR_OUTLINE_OPACITY_MAX)
    } else {
        f64::from(WarOutline::default().opacity)
    }
}
