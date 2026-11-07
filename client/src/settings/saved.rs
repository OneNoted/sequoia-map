//! The settings blob in local storage: the settings, the panels' layout saved beside them, and
//! the migrations that carry an older blob forward.

use gloo_storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::model::{
    DEFAULT_MANUAL_SR_SCALAR, DEFAULT_TERRITORY_OPACITY, NameColor, Settings, WarOutline,
    WarOutlineColor, WarOutlineStyle, clamp_territory_opacity,
};
use crate::app::{
    DEFAULT_SIDEBAR_WIDTH, DEFAULT_WAR_PANEL_WIDTH, clamp_sidebar_width, clamp_war_panel_width,
    migrate_sidebar_width,
};

const KEY: &str = "sequoia_settings_v2";
/// The blob before `KEY`, read only when there is no newer one.
const LEGACY_KEY: &str = "sequoia_settings";

/// Raised when a default changes for viewers who never chose otherwise; [`migrate`] says how.
const DEFAULTS_VERSION: u64 = 4;

/// The outline every save wrote before defaults version 3 unless the member changed it.
const PREVIOUS_DEFAULT_WAR_OUTLINE: WarOutline = WarOutline {
    style: WarOutlineStyle::Dashed,
    color: WarOutlineColor::Stage,
    progress: false,
    width: 1.0,
    opacity: 0.88,
};

/// Saved beside the settings but not among them: the panels as the viewer left them and the
/// leaderboard's sort, which have their own controls outside the menu. Undo and Reset all leave
/// them alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Layout {
    pub sidebar_open: bool,
    pub sidebar_width: f64,
    pub war_panel_open: bool,
    pub war_panel_width: f64,
    pub leaderboard_sort_by_sr: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_open: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            war_panel_open: true,
            war_panel_width: DEFAULT_WAR_PANEL_WIDTH,
            leaderboard_sort_by_sr: false,
        }
    }
}

impl Layout {
    fn clamped(self) -> Self {
        Self {
            sidebar_width: clamp_sidebar_width(migrate_sidebar_width(self.sidebar_width)),
            war_panel_width: clamp_war_panel_width(self.war_panel_width),
            ..self
        }
    }
}

/// The saved settings and layout, or the defaults when nothing usable is saved.
pub(crate) fn load() -> (Settings, Layout) {
    gloo_storage::LocalStorage::get::<Value>(KEY)
        .ok()
        .and_then(read)
        .or_else(|| {
            let legacy = gloo_storage::LocalStorage::get::<LegacySettings>(LEGACY_KEY).ok()?;
            read(serde_json::to_value(legacy).ok()?)
        })
        .unwrap_or_default()
}

pub(crate) fn save(settings: &Settings, layout: &Layout) {
    let _ = gloo_storage::LocalStorage::set(
        KEY,
        Document {
            defaults_version: DEFAULTS_VERSION,
            settings,
            layout,
        },
    );
}

/// The blob as written: one flat object, as every earlier build wrote it.
#[derive(Serialize)]
struct Document<'a> {
    defaults_version: u64,
    #[serde(flatten)]
    settings: &'a Settings,
    #[serde(flatten)]
    layout: &'a Layout,
}

/// A saved blob as this build reads it, or `None` if it cannot be read at all.
fn read(saved: Value) -> Option<(Settings, Layout)> {
    let Value::Object(document) = saved else {
        return None;
    };
    let document = Value::Object(migrate(document));
    let settings = Settings::deserialize(&document).ok()?.clamped();
    let layout = Layout::deserialize(&document).ok()?.clamped();
    Some((settings, layout))
}

/// Carries a blob saved by an older build forward. A setting is moved to a new default only
/// where the blob cannot have held a choice; removing it lets the default fill in.
fn migrate(mut document: Map<String, Value>) -> Map<String, Value> {
    let version = document
        .get("defaults_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if version < 1 {
        for key in [
            "show_resource_icons",
            "show_territory_ornaments",
            "show_debug_info",
        ] {
            document.remove(key);
        }
    }

    if version < 2 {
        for key in ["show_claim_labels", "show_far_zoom_territory_tags"] {
            document.remove(key);
        }
    }

    // Every save wrote the whole outline, so an outline still exactly at the old defaults
    // cannot be told from one chosen to look like them; only that one moves, once. Any other
    // saved outline is a choice and stays.
    if version < 3
        && document
            .get("war_outline")
            .and_then(|outline| serde_json::from_value::<WarOutline>(outline.clone()).ok())
            .is_some_and(|outline| outline.clamped() == PREVIOUS_DEFAULT_WAR_OUTLINE)
    {
        document.remove("war_outline");
    }

    // The fade-only slider of defaults version 3 saved a share of the resting fill's strength;
    // the same look is that share of its opacity.
    if let Some(share) = document
        .remove("territory_opacity")
        .and_then(|v| v.as_f64())
        && version < 4
    {
        let opacity = clamp_territory_opacity(share.clamp(0.0, 1.0) * DEFAULT_TERRITORY_OPACITY);
        document.insert("territory_fill_opacity".into(), opacity.into());
    }

    // Territory names were sized with the guild tags until they had a size of their own.
    if document
        .get("label_scale_static_name")
        .is_none_or(Value::is_null)
        && let Some(tags) = document.get("label_scale_static").cloned()
    {
        document.insert("label_scale_static_name".into(), tags);
    }

    document
}

/// The settings blob before `sequoia_settings_v2`, with the defaults it had.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct LegacySettings {
    show_connections: bool,
    abbreviate_names: bool,
    show_countdown: bool,
    granular_map_time: bool,
    compound_map_time: bool,
    show_names: bool,
    thick_cooldown_borders: bool,
    readable_font: bool,
    bold_connections: bool,
    name_color: NameColor,
    sidebar_open: bool,
    resource_highlight: bool,
    defense_highlight: bool,
    map_intel_enabled: bool,
    show_resource_icons: bool,
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
            readable_font: false,
            bold_connections: false,
            name_color: NameColor::White,
            sidebar_open: false,
            resource_highlight: false,
            defense_highlight: false,
            map_intel_enabled: false,
            show_resource_icons: false,
            manual_sr_scalar: DEFAULT_MANUAL_SR_SCALAR,
            auto_sr_scalar_enabled: true,
            show_leaderboard_sr_gain: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULTS_VERSION, Document, Layout, LegacySettings, read};
    use crate::app::{DEFAULT_SIDEBAR_WIDTH, DEFAULT_WAR_PANEL_WIDTH};
    use crate::settings::model::*;
    use serde_json::json;

    /// What [`super::save`] writes, read back as JSON.
    fn document(settings: &Settings, layout: &Layout) -> serde_json::Value {
        let written = serde_json::to_string(&Document {
            defaults_version: DEFAULTS_VERSION,
            settings,
            layout,
        })
        .unwrap();
        serde_json::from_str(&written).unwrap()
    }

    fn loaded(saved: serde_json::Value) -> (Settings, Layout) {
        read(saved).expect("a readable blob")
    }

    fn settings(saved: serde_json::Value) -> Settings {
        loaded(saved).0
    }

    fn round_trip(settings: Settings) -> Settings {
        loaded(document(&settings, &Layout::default())).0
    }

    fn legacy(saved: LegacySettings) -> (Settings, Layout) {
        loaded(serde_json::to_value(saved).unwrap())
    }

    /// What the build before this module wrote for the defaults, key for key.
    #[test]
    fn the_blob_is_written_as_before() {
        let before: serde_json::Value = serde_json::from_str(r#"{"defaults_version":4,"show_connections":true,"abbreviate_names":true,"show_countdown":false,"granular_map_time":false,"compound_map_time":true,"show_names":false,"show_claim_labels":false,"show_far_zoom_territory_tags":true,"thick_cooldown_borders":true,"bold_connections":false,"connection_style":"Classic","connection_opacity_scale":1.0,"connection_solid_opacity":1.0,"connection_thickness_scale":1.0,"sidebar_open":false,"war_panel_open":true,"war_panel_width":300.0,"show_war_queue":true,"show_war_markers":true,"activity_include_unconfirmed":false,"show_player_heads":true,"player_head_render_head":true,"player_head_render_label":false,"player_head_size":20.0,"resource_highlight":false,"resource_highlight_opacity":0.45,"territory_fill_opacity":0.26,"defense_highlight":false,"map_intel_enabled":false,"show_resource_icons":true,"show_territory_ornaments":false,"hq_crown_style":"NameBelow","cooldown_display":"Fill","hq_crown_scale":1.0,"war_outline":{"style":"Solid","color":"Stage","progress":true,"width":1.0,"opacity":0.88},"manual_sr_scalar":1.5,"auto_sr_scalar_enabled":true,"show_leaderboard_sr_gain":false,"show_leaderboard_sr_value":false,"show_leaderboard_territory_count":true,"show_leaderboard_online":true,"leaderboard_sort_by_sr":false,"heat_mode_enabled":false,"heat_live_source":"all_time","heat_history_basis":"season_cumulative","heat_selected_season_id":null,"readable_font":false,"show_minimap":false,"name_color":"Guild","tag_color":"Guild","label_scale_master":1.0,"label_scale_static":1.0,"label_scale_static_name":1.0,"label_scale_dynamic":1.0,"label_scale_icons":1.0,"sidebar_width":420.0,"show_debug_info":false}"#).unwrap();
        assert_eq!(document(&Settings::default(), &Layout::default()), before);
        assert_eq!(loaded(before), (Settings::default(), Layout::default()));
    }

    #[test]
    fn chosen_settings_and_layout_read_back_the_same() {
        let chosen = Settings {
            show_names: true,
            territory_fill_opacity: 0.63,
            overlay: Overlay::Defense,
            war_outline: WarOutline {
                style: WarOutlineStyle::Dashed,
                width: 1.75,
                opacity: 0.45,
                ..WarOutline::default()
            },
            label_scale_static_name: 1.35,
            heat_selected_season_id: Some(30),
            manual_sr_scalar: 2.25,
            ..Settings::default()
        };
        let layout = Layout {
            sidebar_open: true,
            sidebar_width: 500.0,
            war_panel_open: false,
            war_panel_width: 260.0,
            leaderboard_sort_by_sr: true,
        };
        assert_eq!(loaded(document(&chosen, &layout)), (chosen, layout.clone()));
        let saved = document(&chosen, &layout);
        assert_eq!(saved["defense_highlight"], true);
        assert_eq!(saved["resource_highlight"], false);
        assert_eq!(saved["map_intel_enabled"], false);
    }

    #[test]
    fn a_blob_holding_two_overlays_keeps_the_resource_one() {
        let saved = settings(json!({ "resource_highlight": true, "defense_highlight": true }));
        assert_eq!(saved.overlay, Overlay::Resources);
        assert_eq!(
            settings(json!({ "map_intel_enabled": true })).overlay,
            Overlay::Intel
        );
    }

    #[test]
    fn an_unreadable_blob_reads_as_nothing() {
        assert!(read(json!("settings")).is_none());
        assert!(read(json!({ "show_names": "yes" })).is_none());
    }

    #[test]
    fn settings_without_a_war_panel_open_the_overview_by_default() {
        // Everyone's saved settings predate the war panel, so its defaults have to come
        // from serde rather than from a defaults-version migration.
        let (_, layout) = loaded(json!({}));
        assert!(layout.war_panel_open);
        assert_eq!(layout.war_panel_width, DEFAULT_WAR_PANEL_WIDTH);
    }

    #[test]
    fn territory_opacity_starts_at_the_usual_look_and_keeps_a_saved_choice() {
        assert_eq!(
            settings(json!({})).territory_fill_opacity,
            DEFAULT_TERRITORY_OPACITY
        );
        assert_eq!(
            DEFAULT_TERRITORY_OPACITY as f32,
            sequoia_map_engine::settings::DEFAULT_TERRITORY_OPACITY
        );
        let saved = Settings {
            territory_fill_opacity: 0.9,
            ..Settings::default()
        };
        assert_eq!(round_trip(saved).territory_fill_opacity, 0.9);
        assert_eq!(clamp_territory_opacity(f64::NAN), DEFAULT_TERRITORY_OPACITY);
        assert_eq!(clamp_territory_opacity(-0.5), TERRITORY_OPACITY_MIN);
        assert_eq!(clamp_territory_opacity(3.0), TERRITORY_OPACITY_MAX);
        assert_eq!(TERRITORY_OPACITY_MAX, 1.0);
        assert_eq!(
            settings(json!({ "territory_fill_opacity": 7.0 })).territory_fill_opacity,
            TERRITORY_OPACITY_MAX
        );
    }

    #[test]
    fn a_faded_overlay_saved_by_the_fade_only_slider_keeps_its_look() {
        let migrated = settings(json!({
            "defaults_version": 3,
            "territory_opacity": 0.5,
            "show_minimap": false,
        }));
        assert_eq!(
            migrated.territory_fill_opacity,
            0.5 * DEFAULT_TERRITORY_OPACITY
        );
        assert!(!migrated.show_minimap);
        let written = document(&migrated, &Layout::default());
        assert!(written.get("territory_opacity").is_none());
        assert_eq!(written["defaults_version"], DEFAULTS_VERSION);

        let full = settings(json!({ "defaults_version": 3, "territory_opacity": 1.0 }));
        assert_eq!(full.territory_fill_opacity, DEFAULT_TERRITORY_OPACITY);
        let current = settings(json!({ "defaults_version": 4, "territory_opacity": 0.1 }));
        assert_eq!(current.territory_fill_opacity, DEFAULT_TERRITORY_OPACITY);
    }

    #[test]
    fn the_cooldown_display_defaults_to_the_fill_and_keeps_a_saved_bar() {
        assert_eq!(settings(json!({})).cooldown_display, CooldownDisplay::Fill);
        let saved = Settings {
            cooldown_display: CooldownDisplay::Bar,
            ..Settings::default()
        };
        assert_eq!(round_trip(saved).cooldown_display, CooldownDisplay::Bar);
        let future = settings(json!({ "cooldown_display": "Ring", "show_countdown": true }));
        assert_eq!(future.cooldown_display, CooldownDisplay::Fill);
        assert!(future.show_countdown);
    }

    #[test]
    fn hq_crown_settings_default_to_name_below_and_round_trip() {
        let old = settings(json!({}));
        assert_eq!(old.hq_crown_style, HqCrownStyle::NameBelow);
        assert_eq!(old.hq_crown_scale, DEFAULT_HQ_CROWN_SCALE);
        let saved = round_trip(Settings {
            hq_crown_style: HqCrownStyle::Fill,
            hq_crown_scale: 1.4,
            ..Settings::default()
        });
        assert_eq!(saved.hq_crown_style, HqCrownStyle::Fill);
        assert_eq!(saved.hq_crown_scale, 1.4);
        assert_eq!(clamp_hq_crown_scale(f64::NAN), DEFAULT_HQ_CROWN_SCALE);
        assert_eq!(clamp_hq_crown_scale(9.0), HQ_CROWN_SCALE_MAX);
        let future = settings(json!({ "hq_crown_style": "Banner", "show_names": true }));
        assert_eq!(future.hq_crown_style, HqCrownStyle::NameBelow);
        assert!(future.show_names);
    }

    #[test]
    fn settings_saved_before_later_options_take_their_defaults() {
        // These post-date every saved blob, so serde defaults carry them rather than a
        // defaults-version migration.
        let parsed = settings(json!({}));
        assert!(parsed.show_war_queue);
        assert!(parsed.show_war_markers);
        assert!(!parsed.activity_include_unconfirmed);
        assert!(parsed.show_player_heads);
        assert!(parsed.player_head_render_head);
        assert!(!parsed.player_head_render_label);
        assert_eq!(parsed.player_head_size, DEFAULT_PLAYER_HEAD_SIZE);
        assert!(parsed.auto_sr_scalar_enabled);
        assert_eq!(parsed.overlay, Overlay::Off);
        assert!(parsed.show_resource_icons);
        assert!(!parsed.show_claim_labels);
        assert!(parsed.show_far_zoom_territory_tags);
        assert!(!parsed.show_debug_info);
        assert_eq!(loaded(json!({})).1.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        let saved = round_trip(Settings {
            activity_include_unconfirmed: true,
            ..Settings::default()
        });
        assert!(saved.activity_include_unconfirmed);
    }

    #[test]
    fn saved_settings_keep_or_default_the_resource_highlight_opacity() {
        let old = settings(json!({ "resource_highlight": true }));
        assert_eq!(old.overlay, Overlay::Resources);
        assert_eq!(
            old.resource_highlight_opacity,
            DEFAULT_RESOURCE_HIGHLIGHT_OPACITY
        );
        assert_eq!(
            legacy(LegacySettings::default())
                .0
                .resource_highlight_opacity,
            DEFAULT_RESOURCE_HIGHLIGHT_OPACITY
        );
        let saved = round_trip(Settings {
            resource_highlight_opacity: 0.8,
            ..Settings::default()
        });
        assert_eq!(saved.resource_highlight_opacity, 0.8);
    }

    #[test]
    fn saved_connection_preferences_keep_the_classic_look() {
        // Saved before connection styles existed: classic, with the old choices intact.
        let old = settings(json!({
            "bold_connections": true,
            "connection_opacity_scale": 1.8,
            "connection_thickness_scale": 0.9,
        }));
        assert_eq!(old.connection_style, ConnectionStyle::Classic);
        assert!(old.bold_connections);
        assert_eq!(old.connection_opacity_scale, 1.8);
        assert_eq!(old.connection_thickness_scale, 0.9);
        assert_eq!(
            old.connection_solid_opacity,
            DEFAULT_CONNECTION_SOLID_OPACITY
        );
        assert_eq!(
            legacy(LegacySettings::default()).0.connection_style,
            ConnectionStyle::Classic
        );
        let chosen = Settings {
            connection_style: ConnectionStyle::Guild,
            connection_solid_opacity: 0.6,
            ..Settings::default()
        };
        assert_eq!(
            document(&chosen, &Layout::default())["connection_style"],
            "Guild"
        );
        let saved = round_trip(chosen);
        assert_eq!(saved.connection_style, ConnectionStyle::Guild);
        assert_eq!(saved.connection_solid_opacity, 0.6);
    }

    #[test]
    fn clamps_hold_their_bounds_and_survive_nan() {
        assert_eq!(
            clamp_connection_solid_opacity(f64::NAN),
            DEFAULT_CONNECTION_SOLID_OPACITY
        );
        assert_eq!(
            clamp_connection_solid_opacity(0.0),
            CONNECTION_SOLID_OPACITY_MIN
        );
        assert_eq!(
            clamp_connection_solid_opacity(3.0),
            CONNECTION_SOLID_OPACITY_MAX
        );
        assert_eq!(
            clamp_connection_opacity_scale(f64::NAN),
            DEFAULT_CONNECTION_OPACITY_SCALE
        );
        assert_eq!(
            clamp_connection_thickness_scale(f64::NAN),
            DEFAULT_CONNECTION_THICKNESS_SCALE
        );
        assert_eq!(
            clamp_connection_thickness_scale(9.0),
            CONNECTION_THICKNESS_SCALE_MAX
        );
        assert_eq!(
            clamp_resource_highlight_opacity(0.0),
            RESOURCE_HIGHLIGHT_OPACITY_MIN
        );
        assert_eq!(clamp_resource_highlight_opacity(0.6), 0.6);
        assert_eq!(
            clamp_resource_highlight_opacity(4.0),
            RESOURCE_HIGHLIGHT_OPACITY_MAX
        );
        assert_eq!(
            clamp_resource_highlight_opacity(f64::NAN),
            DEFAULT_RESOURCE_HIGHLIGHT_OPACITY
        );
        assert_eq!(clamp_player_head_size(0.0), PLAYER_HEAD_SIZE_MIN);
        assert_eq!(clamp_player_head_size(1000.0), PLAYER_HEAD_SIZE_MAX);
        assert_eq!(clamp_player_head_size(24.0), 24.0);
        // A hand-edited blob can carry `NaN`, which would draw nothing at all rather than
        // being pinned to a bound.
        assert_eq!(clamp_player_head_size(f64::NAN), DEFAULT_PLAYER_HEAD_SIZE);
    }

    #[test]
    fn resource_highlight_opacity_bounds_match_the_renderer() {
        use sequoia_map_engine::settings;
        for (host, renderer) in [
            (
                DEFAULT_RESOURCE_HIGHLIGHT_OPACITY,
                settings::DEFAULT_RESOURCE_HIGHLIGHT_OPACITY,
            ),
            (
                RESOURCE_HIGHLIGHT_OPACITY_MIN,
                settings::RESOURCE_HIGHLIGHT_OPACITY_MIN,
            ),
            (
                RESOURCE_HIGHLIGHT_OPACITY_MAX,
                settings::RESOURCE_HIGHLIGHT_OPACITY_MAX,
            ),
        ] {
            assert_eq!(host as f32, renderer);
        }
    }

    #[test]
    fn a_saved_minimap_choice_survives_loading_and_migrations() {
        assert!(
            settings(json!({ "defaults_version": DEFAULTS_VERSION, "show_minimap": true }))
                .show_minimap
        );
        assert!(settings(json!({ "defaults_version": 1, "show_minimap": true })).show_minimap);
        assert!(!settings(json!({ "defaults_version": 2 })).show_minimap);
        assert!(!legacy(LegacySettings::default()).0.show_minimap);
    }

    #[test]
    fn only_the_exact_old_default_outline_moves_to_solid_progress() {
        let migrate = |version: u64, outline: serde_json::Value| {
            settings(json!({ "defaults_version": version, "war_outline": outline })).war_outline
        };

        let old_default = json!({"style": "Dashed", "color": "Stage", "progress": false, "width": 1.0, "opacity": 0.88});
        assert_eq!(migrate(2, old_default), WarOutline::default());

        let customized = json!({"style": "Dashed", "color": "Urgency", "progress": false, "width": 1.5, "opacity": 0.6});
        let kept = migrate(2, customized);
        assert_eq!(
            (
                kept.style,
                kept.color,
                kept.progress,
                kept.width,
                kept.opacity
            ),
            (
                WarOutlineStyle::Dashed,
                WarOutlineColor::Urgency,
                false,
                1.5,
                0.6
            )
        );

        let chosen = migrate(3, json!({"style": "Dashed", "progress": false}));
        assert_eq!(
            (chosen.style, chosen.progress),
            (WarOutlineStyle::Dashed, false)
        );
    }

    #[test]
    fn settings_v1_migration_preserves_unrelated_user_choices_and_applies_label_defaults() {
        let (saved, layout) = loaded(json!({
            "defaults_version": 1,
            "sidebar_width": 420.0,
            "show_minimap": false,
            "show_resource_icons": false,
            "show_claim_labels": true,
            "show_far_zoom_territory_tags": false,
            "show_territory_ornaments": true,
            "show_debug_info": true,
        }));
        assert_eq!(layout.sidebar_width, 420.0);
        assert!(!saved.show_minimap);
        assert!(!saved.show_resource_icons);
        assert!(!saved.show_claim_labels);
        assert!(saved.show_far_zoom_territory_tags);
        assert!(saved.show_territory_ornaments);
        assert!(saved.show_debug_info);
    }

    #[test]
    fn settings_v0_migration_keeps_previous_default_overrides() {
        let saved = settings(json!({
            "defaults_version": 0,
            "show_resource_icons": false,
            "show_claim_labels": true,
            "show_far_zoom_territory_tags": false,
            "show_territory_ornaments": true,
            "show_debug_info": true,
        }));
        assert!(saved.show_resource_icons);
        assert!(!saved.show_claim_labels);
        assert!(saved.show_far_zoom_territory_tags);
        assert!(!saved.show_territory_ornaments);
        assert!(!saved.show_debug_info);
    }

    #[test]
    fn current_settings_preserve_user_label_choices() {
        let saved = settings(json!({
            "defaults_version": DEFAULTS_VERSION,
            "show_claim_labels": true,
            "show_far_zoom_territory_tags": false,
        }));
        assert!(saved.show_claim_labels);
        assert!(!saved.show_far_zoom_territory_tags);
    }

    #[test]
    fn territory_names_saved_without_a_size_of_their_own_keep_the_tag_size() {
        let saved = settings(json!({ "label_scale_static": 1.4 }));
        assert_eq!(saved.label_scale_static_name, 1.4);
        let saved = settings(json!({ "label_scale_static": 1.4, "label_scale_static_name": null }));
        assert_eq!(saved.label_scale_static_name, 1.4);
        let saved = settings(json!({ "label_scale_static": 1.4, "label_scale_static_name": 0.8 }));
        assert_eq!(saved.label_scale_static_name, 0.8);
    }

    #[test]
    fn legacy_settings_migration_preserves_old_key_choices_and_applies_current_default_overrides() {
        let (migrated, layout) = legacy(LegacySettings {
            show_connections: false,
            sidebar_open: true,
            resource_highlight: true,
            show_resource_icons: false,
            manual_sr_scalar: 2.0,
            name_color: NameColor::Gold,
            ..LegacySettings::default()
        });
        assert!(!migrated.show_connections);
        assert!(layout.sidebar_open);
        assert_eq!(migrated.overlay, Overlay::Resources);
        assert_eq!(migrated.manual_sr_scalar, 2.0);
        assert_eq!(migrated.name_color, NameColor::Gold);
        assert!(migrated.show_resource_icons);
        assert!(!migrated.show_claim_labels);
        assert!(migrated.show_far_zoom_territory_tags);
        // Its own defaults, where they differed.
        assert_eq!(
            legacy(LegacySettings::default()).0.name_color,
            NameColor::White
        );
    }
}
