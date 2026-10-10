//! Renderer-facing settings and the invalidation matrix derived from them.

use crate::scene::Rebuild;

/// Colour scheme applied to territory name labels.
///
/// The serde representation is PascalCase (`"White"`, `"Guild"`, ...) and is
/// load-bearing: it is persisted in `localStorage` under `sequoia_settings_v2`.
/// Do not add `rename_all` here without a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NameColor {
    White,  // rgba(220, 218, 210, 0.88) — current default
    Guild,  // per-territory guild color (brightened), same as tag line
    Gold,   // rgba(245, 197, 66, 0.88) — matches app accent
    Copper, // rgba(181, 103, 39, 0.88) — warm copper
    Muted,  // rgba(120, 116, 112, 0.78) — subtle/subdued
}

/// How connection lines are drawn; see [`crate::connections`].
///
/// Persisted in `localStorage` (`sequoia_settings_v2`) as PascalCase, like [`NameColor`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConnectionStyle {
    /// Soft hairline bands: faint white, guild-tinted when bold. The original look.
    #[default]
    Classic,
    /// Solid white lines.
    White,
    /// Solid lines in the guild colour; edges between two guilds are split at the middle.
    Guild,
}

impl ConnectionStyle {
    /// Whether the connection vertices depend on the main camera's scale (classic hairline
    /// spacing and zoom fade). Solid strips are widened in screen space instead.
    pub fn follows_zoom(self) -> bool {
        self == ConnectionStyle::Classic
    }
}

/// How the map is drawn: every display option a host can set. Hosts rebuild this whenever
/// one of their settings changes; [`RenderSettings::invalidates`] decides what that costs.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderSettings {
    // Territory fills and borders.
    pub thick_cooldown_borders: bool,
    /// Draws every territory as if its cooldown had expired.
    pub suppress_cooldown_visuals: bool,
    pub resource_highlight: bool,
    /// Base fill alpha of resource-highlighted territories, before hover and selection.
    pub resource_highlight_opacity: f32,
    pub defense_highlight: bool,
    /// Added to every territory fill alpha.
    pub fill_alpha_boost: f32,

    // Connection lines.
    pub show_connections: bool,
    pub connection_style: ConnectionStyle,
    /// Classic: guild-tinted, stronger and wider. Solid: twice as wide.
    pub bold_connections: bool,
    /// Classic only: multiplies the classic opacities.
    pub connection_opacity_scale: f32,
    /// Solid styles only: their opacity, 0 to 1.
    pub connection_solid_opacity: f32,
    /// Multiplies the line width.
    pub connection_thickness_scale: f32,
    /// Classic connections fade in between these two viewport scales.
    pub connection_zoom_fade: (f32, f32),

    // Static labels: guild tags, territory names and claim labels.
    pub show_names: bool,
    pub abbreviate_names: bool,
    pub show_claim_labels: bool,
    pub show_far_zoom_territory_tags: bool,
    pub name_color: NameColor,
    pub tag_color: NameColor,
    pub readable_font: bool,

    // Timers and icons.
    pub show_countdown: bool,
    pub granular_map_time: bool,
    pub compound_map_time: bool,
    pub show_resource_icons: bool,
    pub show_territory_ornaments: bool,

    pub label_scales: LabelScales,
}

/// Faintest base alpha of resource-highlighted territories the slider allows.
pub const RESOURCE_HIGHLIGHT_OPACITY_MIN: f32 = 0.15;
/// Firmest base alpha; hover, selection and the far-zoom boost still add to it.
pub const RESOURCE_HIGHLIGHT_OPACITY_MAX: f32 = 0.90;
/// Initial base alpha, a little firmer than the 0.34 used before the slider existed.
pub const DEFAULT_RESOURCE_HIGHLIGHT_OPACITY: f32 = 0.45;
/// Defense tiers keep the fixed overlay alpha.
const DEFENSE_HIGHLIGHT_OPACITY: f32 = 0.34;

/// User label size multipliers. Each group is multiplied by the master scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelScales {
    pub master: f32,
    pub static_tag: f32,
    pub static_name: f32,
    pub dynamic: f32,
    pub icons: f32,
}

impl LabelScales {
    pub fn static_tag(&self) -> f32 {
        self.effective(self.static_tag)
    }

    pub fn static_name(&self) -> f32 {
        self.effective(self.static_name)
    }

    pub fn dynamic(&self) -> f32 {
        self.effective(self.dynamic)
    }

    pub fn icons(&self) -> f32 {
        self.effective(self.icons)
    }

    fn effective(&self, group: f32) -> f32 {
        let master = if self.master.is_finite() {
            self.master
        } else {
            1.0
        };
        let group = if group.is_finite() { group } else { 1.0 };
        (master * group).clamp(0.5, 4.0)
    }
}

impl RenderSettings {
    /// Base fill alpha of a territory carrying a resource or defense overlay.
    pub fn overlay_fill_alpha(&self) -> f32 {
        if self.defense_highlight || !self.resource_highlight {
            return DEFENSE_HIGHLIGHT_OPACITY;
        }
        if self.resource_highlight_opacity.is_finite() {
            self.resource_highlight_opacity.clamp(
                RESOURCE_HIGHLIGHT_OPACITY_MIN,
                RESOURCE_HIGHLIGHT_OPACITY_MAX,
            )
        } else {
            DEFAULT_RESOURCE_HIGHLIGHT_OPACITY
        }
    }

    /// The cached outputs that changing from `previous` to `self` makes stale.
    pub fn invalidates(&self, previous: &Self) -> Rebuild {
        let territory_style = self.thick_cooldown_borders != previous.thick_cooldown_borders
            || self.suppress_cooldown_visuals != previous.suppress_cooldown_visuals
            || self.resource_highlight != previous.resource_highlight
            || self.resource_highlight_opacity != previous.resource_highlight_opacity
            || self.defense_highlight != previous.defense_highlight
            || self.fill_alpha_boost != previous.fill_alpha_boost;
        let connection_style = self.show_connections != previous.show_connections
            || self.connection_style != previous.connection_style
            || self.bold_connections != previous.bold_connections
            || self.connection_opacity_scale != previous.connection_opacity_scale
            || self.connection_solid_opacity != previous.connection_solid_opacity
            || self.connection_thickness_scale != previous.connection_thickness_scale
            || self.connection_zoom_fade != previous.connection_zoom_fade;
        // Static text only.
        let tag_style = self.abbreviate_names != previous.abbreviate_names
            || self.show_claim_labels != previous.show_claim_labels
            || self.show_far_zoom_territory_tags != previous.show_far_zoom_territory_tags
            || self.name_color != previous.name_color
            || self.tag_color != previous.tag_color;
        // Territory names push timers and icons down.
        let names = self.show_names != previous.show_names;
        let font = self.readable_font != previous.readable_font;
        let timers = self.show_countdown != previous.show_countdown
            || self.granular_map_time != previous.granular_map_time
            || self.compound_map_time != previous.compound_map_time;
        // Resource icons lift every label above them.
        let resource_icons = self.show_resource_icons != previous.show_resource_icons;
        let ornaments = self.show_territory_ornaments != previous.show_territory_ornaments;
        let (scales, previous_scales) = (&self.label_scales, &previous.label_scales);
        let static_scale = scales.master != previous_scales.master
            || scales.static_tag != previous_scales.static_tag
            || scales.static_name != previous_scales.static_name;
        let dynamic_scale = scales.dynamic != previous_scales.dynamic;
        let icon_scale = scales.icons != previous_scales.icons;

        Rebuild {
            territories: territory_style,
            connections: connection_style,
            static_labels: tag_style || names || font || resource_icons || static_scale,
            dynamic_labels: names
                || font
                || timers
                || resource_icons
                || static_scale
                || dynamic_scale,
            icons: names
                || timers
                || resource_icons
                || ornaments
                || static_scale
                || dynamic_scale
                || icon_scale,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        DEFAULT_RESOURCE_HIGHLIGHT_OPACITY, LabelScales, NameColor, RESOURCE_HIGHLIGHT_OPACITY_MAX,
        RESOURCE_HIGHLIGHT_OPACITY_MIN, RenderSettings,
    };
    use crate::scene::Rebuild;

    pub(crate) fn settings() -> RenderSettings {
        RenderSettings {
            thick_cooldown_borders: true,
            suppress_cooldown_visuals: false,
            resource_highlight: false,
            resource_highlight_opacity: DEFAULT_RESOURCE_HIGHLIGHT_OPACITY,
            defense_highlight: false,
            fill_alpha_boost: 0.0,
            show_connections: true,
            connection_style: super::ConnectionStyle::Classic,
            bold_connections: false,
            connection_opacity_scale: 1.0,
            connection_solid_opacity: 1.0,
            connection_thickness_scale: 1.0,
            connection_zoom_fade: (0.15, 0.45),
            show_names: false,
            abbreviate_names: true,
            show_claim_labels: false,
            show_far_zoom_territory_tags: true,
            name_color: NameColor::Guild,
            tag_color: NameColor::Guild,
            readable_font: false,
            show_countdown: false,
            granular_map_time: false,
            compound_map_time: true,
            show_resource_icons: true,
            show_territory_ornaments: false,
            label_scales: LabelScales {
                master: 1.0,
                static_tag: 1.0,
                static_name: 1.0,
                dynamic: 1.0,
                icons: 1.0,
            },
        }
    }

    fn after(change: impl FnOnce(&mut RenderSettings)) -> Rebuild {
        let mut next = settings();
        change(&mut next);
        next.invalidates(&settings())
    }

    #[test]
    fn unchanged_settings_invalidate_nothing() {
        assert_eq!(settings().invalidates(&settings()), Rebuild::NONE);
    }

    #[test]
    fn territory_and_connection_styles_stay_in_their_own_buffers() {
        assert_eq!(
            after(|s| s.defense_highlight = true),
            Rebuild {
                territories: true,
                ..Rebuild::NONE
            }
        );
        assert_eq!(
            after(|s| s.resource_highlight_opacity = 0.8),
            Rebuild {
                territories: true,
                ..Rebuild::NONE
            }
        );
        let connections_only = Rebuild {
            connections: true,
            ..Rebuild::NONE
        };
        assert_eq!(
            after(|s| s.connection_zoom_fade = (0.1, 0.3)),
            connections_only
        );
        assert_eq!(
            after(|s| s.connection_style = super::ConnectionStyle::Guild),
            connections_only
        );
        assert_eq!(
            after(|s| s.connection_solid_opacity = 0.5),
            connections_only
        );
    }

    #[test]
    fn label_settings_reach_exactly_the_layouts_that_read_them() {
        let tags_only = Rebuild {
            static_labels: true,
            ..Rebuild::NONE
        };
        assert_eq!(after(|s| s.tag_color = NameColor::Gold), tags_only);
        assert_eq!(after(|s| s.abbreviate_names = false), tags_only);

        // Timers move icons but never the static tags.
        assert_eq!(
            after(|s| s.show_countdown = true),
            Rebuild {
                dynamic_labels: true,
                icons: true,
                ..Rebuild::NONE
            }
        );
        // A new font changes glyph metrics, not icon placement.
        assert_eq!(
            after(|s| s.readable_font = true),
            Rebuild {
                static_labels: true,
                dynamic_labels: true,
                ..Rebuild::NONE
            }
        );
        // Names, resource icons and static scales shift every label layer.
        for change in [
            (|s: &mut RenderSettings| s.show_names = true) as fn(&mut RenderSettings),
            |s| s.show_resource_icons = false,
            |s| s.label_scales.master = 1.5,
        ] {
            assert_eq!(
                after(change),
                Rebuild {
                    static_labels: true,
                    dynamic_labels: true,
                    icons: true,
                    ..Rebuild::NONE
                }
            );
        }
        assert_eq!(
            after(|s| s.label_scales.icons = 1.4),
            Rebuild {
                icons: true,
                ..Rebuild::NONE
            }
        );
    }

    #[test]
    fn resource_highlight_opacity_only_drives_resource_overlays() {
        let mut resources = settings();
        resources.resource_highlight = true;
        resources.resource_highlight_opacity = 0.8;
        assert_eq!(resources.overlay_fill_alpha(), 0.8);
        resources.resource_highlight_opacity = 5.0;
        assert_eq!(
            resources.overlay_fill_alpha(),
            RESOURCE_HIGHLIGHT_OPACITY_MAX
        );
        resources.resource_highlight_opacity = 0.0;
        assert_eq!(
            resources.overlay_fill_alpha(),
            RESOURCE_HIGHLIGHT_OPACITY_MIN
        );
        resources.resource_highlight_opacity = f32::NAN;
        assert_eq!(
            resources.overlay_fill_alpha(),
            DEFAULT_RESOURCE_HIGHLIGHT_OPACITY
        );

        // Defense tiers keep their fixed fill whatever the slider says.
        let mut defense = settings();
        defense.defense_highlight = true;
        defense.resource_highlight_opacity = 0.8;
        assert_eq!(defense.overlay_fill_alpha(), 0.34);
    }

    #[test]
    fn effective_label_scales_are_clamped_and_ignore_garbage() {
        let scales = LabelScales {
            master: 2.0,
            static_tag: 3.0,
            static_name: f32::NAN,
            dynamic: 0.1,
            icons: 1.0,
        };
        assert_eq!(scales.static_tag(), 4.0);
        assert_eq!(scales.static_name(), 2.0);
        assert_eq!(scales.dynamic(), 0.5);
        assert_eq!(scales.icons(), 2.0);
    }

    /// The persisted form is PascalCase; users have settings saved against it.
    #[test]
    fn name_color_serde_representation_is_pascal_case() {
        for (value, expected) in [
            (NameColor::White, "\"White\""),
            (NameColor::Guild, "\"Guild\""),
            (NameColor::Gold, "\"Gold\""),
            (NameColor::Copper, "\"Copper\""),
            (NameColor::Muted, "\"Muted\""),
        ] {
            let encoded = serde_json::to_string(&value).expect("serialize");
            assert_eq!(encoded, expected);
            let decoded: NameColor = serde_json::from_str(expected).expect("deserialize");
            assert_eq!(decoded, value);
        }
    }
}
