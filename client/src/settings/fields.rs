//! Each setting a control shows: what it is called and where it lives in [`Settings`].
//!
//! A control reads and writes its setting through one of these, a reset puts it back through
//! it, and Undo names a change by it. Some are views over several saved values, such as the
//! overlay or the time held, and change them together.

use super::model::*;

/// One setting as a control shows it.
pub(crate) struct Setting<T: 'static> {
    /// What the menu calls it, and what Undo names a change to it.
    pub name: &'static str,
    get: fn(&Settings) -> T,
    put: fn(&mut Settings, T),
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Setting<T> {}

impl<T> Setting<T> {
    pub(crate) fn get(self, settings: &Settings) -> T {
        (self.get)(settings)
    }

    /// Writes `value`, brought into the setting's range.
    pub(crate) fn set(self, settings: &mut Settings, value: T) {
        (self.put)(settings, value);
    }
}

/// A setting that can go back to its default, whatever its type: what a reset needs.
pub(crate) trait Defaulted: Sync {
    fn is_default(&self, current: &Settings, defaults: &Settings) -> bool;
    fn restore(&self, current: &mut Settings, defaults: &Settings);
}

impl<T: PartialEq> Defaulted for Setting<T> {
    fn is_default(&self, current: &Settings, defaults: &Settings) -> bool {
        self.get(current) == self.get(defaults)
    }

    fn restore(&self, current: &mut Settings, defaults: &Settings) {
        self.set(current, self.get(defaults));
    }
}

macro_rules! setting {
    ($name:literal, $field:ident) => {
        Setting {
            name: $name,
            get: |settings| settings.$field,
            put: |settings, value| settings.$field = value,
        }
    };
    ($name:literal, $field:ident, $clamp:path) => {
        Setting {
            name: $name,
            get: |settings| settings.$field,
            put: |settings, value| settings.$field = $clamp(value),
        }
    };
}

pub(crate) const TERRITORY_OPACITY: Setting<f64> = setting!(
    "Territory opacity",
    territory_fill_opacity,
    clamp_territory_opacity
);
pub(crate) const OVERLAY: Setting<Overlay> = setting!("Overlay", overlay);
pub(crate) const RESOURCE_OPACITY: Setting<f64> = setting!(
    "Resource opacity",
    resource_highlight_opacity,
    clamp_resource_highlight_opacity
);
pub(crate) const HEAT_MAP: Setting<bool> = setting!("Heat Map", heat_mode_enabled);
/// The live heat map's period: the season (`true`) or all time.
pub(crate) const HEAT_LIVE_SEASON: Setting<bool> = Setting {
    name: "Heat period",
    get: |settings| settings.heat_live_source == HeatLiveSource::Season,
    put: |settings, season| {
        settings.heat_live_source = if season {
            HeatLiveSource::Season
        } else {
            HeatLiveSource::AllTime
        }
    },
};
/// History's heat map period, kept apart from the live one.
pub(crate) const HEAT_HISTORY_SEASON: Setting<bool> = Setting {
    name: "Heat period",
    get: |settings| settings.heat_history_basis == HeatHistoryBasis::SeasonCumulative,
    put: |settings, season| {
        settings.heat_history_basis = if season {
            HeatHistoryBasis::SeasonCumulative
        } else {
            HeatHistoryBasis::AllTimeCumulative
        }
    },
};
pub(crate) const HEAT_SEASON: Setting<Option<i32>> = setting!("Season", heat_selected_season_id);
pub(crate) const CONNECTIONS: Setting<bool> = setting!("Connections", show_connections);
pub(crate) const RESOURCE_ICONS: Setting<bool> = setting!("Resource Icons", show_resource_icons);
pub(crate) const CORNER_ORNAMENTS: Setting<bool> =
    setting!("Corner Ornaments", show_territory_ornaments);
pub(crate) const MINIMAP: Setting<bool> = setting!("Minimap", show_minimap);

pub(crate) const TERRITORY_NAMES: Setting<bool> = setting!("Territory Names", show_names);
pub(crate) const ABBREVIATE_NAMES: Setting<bool> = setting!("Abbreviate Names", abbreviate_names);
pub(crate) const ZOOMED_OUT: Setting<ZoomedOutLabels> = Setting {
    name: "When zoomed out",
    get: |settings| {
        ZoomedOutLabels::from_flags(
            settings.show_claim_labels,
            settings.show_far_zoom_territory_tags,
        )
    },
    put: |settings, choice| {
        (
            settings.show_claim_labels,
            settings.show_far_zoom_territory_tags,
        ) = choice.flags(settings.show_far_zoom_territory_tags);
    },
};
pub(crate) const READABLE_FONT: Setting<bool> = setting!("Readable Font", readable_font);
pub(crate) const NAME_COLOR: Setting<NameColor> = setting!("Territory name color", name_color);
pub(crate) const TAG_COLOR: Setting<NameColor> = setting!("Guild tag color", tag_color);
pub(crate) const HQ_CROWN: Setting<HqCrownStyle> = setting!("HQ crown", hq_crown_style);
pub(crate) const CROWN_SIZE: Setting<f64> =
    setting!("Crown size", hq_crown_scale, clamp_hq_crown_scale);

pub(crate) const TIME_HELD: Setting<TimeHeld> = Setting {
    name: "Time held",
    get: |settings| TimeHeld::from_flags(settings.granular_map_time, settings.compound_map_time),
    put: |settings, choice| {
        (settings.granular_map_time, settings.compound_map_time) =
            choice.flags(settings.compound_map_time);
    },
};
pub(crate) const COOLDOWN_COUNTDOWN: Setting<bool> = setting!("Cooldown Countdown", show_countdown);
pub(crate) const THICK_COOLDOWN_BORDERS: Setting<bool> =
    setting!("Thick Cooldown Borders", thick_cooldown_borders);
pub(crate) const COOLDOWN_DISPLAY: Setting<CooldownDisplay> =
    setting!("Cooldown", cooldown_display);

pub(crate) const WAR_PANEL: Setting<bool> = setting!("War Panel", show_war_queue);
pub(crate) const WAR_MARKERS: Setting<bool> = setting!("War Markers", show_war_markers);
pub(crate) const UNCONFIRMED_ACTIVITY: Setting<bool> =
    setting!("Unconfirmed Activity", activity_include_unconfirmed);
pub(crate) const WAR_OUTLINE_STYLE: Setting<WarOutlineStyle> = Setting {
    name: "War outline",
    get: |settings| settings.war_outline.style,
    put: |settings, style| settings.war_outline.style = style,
};
pub(crate) const WAR_OUTLINE_COLOR: Setting<WarOutlineColor> = Setting {
    name: "Outline color",
    get: |settings| settings.war_outline.color,
    put: |settings, color| settings.war_outline.color = color,
};
pub(crate) const WAR_OUTLINE_PROGRESS: Setting<bool> = Setting {
    name: "Queue Progress",
    get: |settings| settings.war_outline.progress,
    put: |settings, progress| settings.war_outline.progress = progress,
};
pub(crate) const WAR_OUTLINE_WIDTH: Setting<f64> = Setting {
    name: "Outline width",
    get: |settings| f64::from(settings.war_outline.width),
    put: |settings, width| settings.war_outline.width = clamp_war_outline_width(width) as f32,
};
pub(crate) const WAR_OUTLINE_OPACITY: Setting<f64> = Setting {
    name: "Outline opacity",
    get: |settings| f64::from(settings.war_outline.opacity),
    put: |settings, opacity| {
        settings.war_outline.opacity = clamp_war_outline_opacity(opacity) as f32;
    },
};
pub(crate) const TEAMMATE_HEADS: Setting<bool> = setting!("Teammate Heads", show_player_heads);
pub(crate) const TEAMMATE_FACES: Setting<bool> = setting!("Show Faces", player_head_render_head);
pub(crate) const TEAMMATE_NAMES: Setting<bool> =
    setting!("Teammate names", player_head_render_label);
pub(crate) const HEAD_SIZE: Setting<f64> =
    setting!("Head size", player_head_size, clamp_player_head_size);

pub(crate) const LEADERBOARD_TERRITORIES: Setting<bool> =
    setting!("Territories", show_leaderboard_territory_count);
pub(crate) const LEADERBOARD_ONLINE: Setting<bool> =
    setting!("Online Members", show_leaderboard_online);
pub(crate) const LEADERBOARD_SR_RATE: Setting<bool> = setting!("SR Rate", show_leaderboard_sr_gain);
pub(crate) const LEADERBOARD_SEASON_RATING: Setting<bool> =
    setting!("Season Rating", show_leaderboard_sr_value);
pub(crate) const ESTIMATE_SR_SCALAR: Setting<bool> =
    setting!("Estimate SR Scalar", auto_sr_scalar_enabled);
pub(crate) const MANUAL_SR_SCALAR: Setting<f64> = setting!(
    "Manual SR Scalar",
    manual_sr_scalar,
    crate::season_scalar::clamp_manual_scalar
);

pub(crate) const LABEL_SIZE: Setting<f64> =
    setting!("Label size", label_scale_master, clamp_label_scale_master);
pub(crate) const GUILD_TAG_SIZE: Setting<f64> = setting!(
    "Guild tag size",
    label_scale_static,
    clamp_label_scale_group
);
pub(crate) const TERRITORY_NAME_SIZE: Setting<f64> = setting!(
    "Territory name size",
    label_scale_static_name,
    clamp_label_scale_group
);
pub(crate) const TIMER_SIZE: Setting<f64> =
    setting!("Timer size", label_scale_dynamic, clamp_label_scale_group);
pub(crate) const ICON_SIZE: Setting<f64> =
    setting!("Icon size", label_scale_icons, clamp_label_scale_group);
pub(crate) const LABEL_SIZES: &[&dyn Defaulted] = &[
    &LABEL_SIZE,
    &GUILD_TAG_SIZE,
    &TERRITORY_NAME_SIZE,
    &TIMER_SIZE,
    &ICON_SIZE,
];

pub(crate) const CONNECTION_STYLE: Setting<ConnectionStyle> =
    setting!("Connection style", connection_style);
pub(crate) const BOLD_CONNECTIONS: Setting<bool> = setting!("Bold Connections", bold_connections);
/// The classic lines' opacity, relative to the classic look.
pub(crate) const CLASSIC_CONNECTION_OPACITY: Setting<f64> = setting!(
    "Connection opacity",
    connection_opacity_scale,
    clamp_connection_opacity_scale
);
/// The solid lines' opacity.
pub(crate) const SOLID_CONNECTION_OPACITY: Setting<f64> = setting!(
    "Connection opacity",
    connection_solid_opacity,
    clamp_connection_solid_opacity
);
pub(crate) const CONNECTION_THICKNESS: Setting<f64> = setting!(
    "Connection thickness",
    connection_thickness_scale,
    clamp_connection_thickness_scale
);
pub(crate) const CONNECTION_LINES: &[&dyn Defaulted] = &[
    &CONNECTION_STYLE,
    &BOLD_CONNECTIONS,
    &CLASSIC_CONNECTION_OPACITY,
    &SOLID_CONNECTION_OPACITY,
    &CONNECTION_THICKNESS,
];

pub(crate) const DIAGNOSTICS: Setting<bool> = setting!("Diagnostics", show_debug_info);

/// What labels the zoomed-out map shows. Guild names (one per connected claim) replace the
/// per-territory tags, so the stored pair `(claim labels, far-zoom tags)` is one choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZoomedOutLabels {
    GuildNames,
    TerritoryTags,
    Off,
}

impl ZoomedOutLabels {
    fn from_flags(claim_labels: bool, far_zoom_tags: bool) -> Self {
        if claim_labels {
            ZoomedOutLabels::GuildNames
        } else if far_zoom_tags {
            ZoomedOutLabels::TerritoryTags
        } else {
            ZoomedOutLabels::Off
        }
    }

    /// New `(claim labels, far-zoom tags)`. Choosing guild names keeps the tag setting,
    /// which is unused until guild names are turned off again.
    fn flags(self, far_zoom_tags: bool) -> (bool, bool) {
        match self {
            ZoomedOutLabels::GuildNames => (true, far_zoom_tags),
            ZoomedOutLabels::TerritoryTags => (false, true),
            ZoomedOutLabels::Off => (false, false),
        }
    }
}

/// How the time a guild has held a territory is written. Exact overrides the compact
/// format, so the stored pair `(exact, compact)` is one choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeHeld {
    Off,
    Compact,
    Exact,
}

impl TimeHeld {
    fn from_flags(exact: bool, compact: bool) -> Self {
        if exact {
            TimeHeld::Exact
        } else if compact {
            TimeHeld::Compact
        } else {
            TimeHeld::Off
        }
    }

    /// New `(exact, compact)`. Exact keeps the compact setting it overrides.
    fn flags(self, compact: bool) -> (bool, bool) {
        match self {
            TimeHeld::Off => (false, false),
            TimeHeld::Compact => (false, true),
            TimeHeld::Exact => (true, compact),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(change: impl FnOnce(&mut Settings)) -> Settings {
        let mut settings = Settings::default();
        change(&mut settings);
        settings
    }

    #[test]
    fn zoomed_out_labels_match_what_the_renderer_draws() {
        let shown = |claims, tags| {
            ZOOMED_OUT.get(&with(|s| {
                s.show_claim_labels = claims;
                s.show_far_zoom_territory_tags = tags;
            }))
        };
        // Guild names replace the tags whatever the tag flag says.
        assert_eq!(shown(true, false), ZoomedOutLabels::GuildNames);
        assert_eq!(shown(true, true), ZoomedOutLabels::GuildNames);
        assert_eq!(shown(false, true), ZoomedOutLabels::TerritoryTags);
        assert_eq!(shown(false, false), ZoomedOutLabels::Off);
        // Picking guild names leaves the dormant tag flag alone.
        let mut settings = with(|s| s.show_far_zoom_territory_tags = false);
        ZOOMED_OUT.set(&mut settings, ZoomedOutLabels::GuildNames);
        assert_eq!(
            (
                settings.show_claim_labels,
                settings.show_far_zoom_territory_tags
            ),
            (true, false)
        );
        ZOOMED_OUT.set(&mut settings, ZoomedOutLabels::TerritoryTags);
        assert_eq!(
            (
                settings.show_claim_labels,
                settings.show_far_zoom_territory_tags
            ),
            (false, true)
        );
    }

    #[test]
    fn time_held_matches_the_renderer_precedence() {
        let shown = |exact, compact| {
            TIME_HELD.get(&with(|s| {
                s.granular_map_time = exact;
                s.compound_map_time = compact;
            }))
        };
        // Exact overrides the compact format.
        assert_eq!(shown(true, true), TimeHeld::Exact);
        assert_eq!(shown(true, false), TimeHeld::Exact);
        assert_eq!(shown(false, true), TimeHeld::Compact);
        assert_eq!(shown(false, false), TimeHeld::Off);
        for choice in [TimeHeld::Off, TimeHeld::Compact, TimeHeld::Exact] {
            let mut settings = Settings::default();
            TIME_HELD.set(&mut settings, choice);
            assert_eq!(TIME_HELD.get(&settings), choice);
        }
        // Exact keeps the compact choice it overrides.
        let mut settings = with(|s| s.compound_map_time = false);
        TIME_HELD.set(&mut settings, TimeHeld::Exact);
        assert!(!settings.compound_map_time);
    }

    #[test]
    fn a_reset_puts_back_exactly_the_saved_defaults() {
        let defaults = Settings::default();
        let mut changed = with(|s| {
            s.show_claim_labels = true;
            s.show_far_zoom_territory_tags = false;
            s.granular_map_time = true;
            s.compound_map_time = false;
            s.overlay = Overlay::Intel;
            s.war_outline.width = 1.5;
            s.show_names = true;
        });
        for setting in [
            &ZOOMED_OUT as &dyn Defaulted,
            &TIME_HELD,
            &OVERLAY,
            &WAR_OUTLINE_WIDTH,
        ] {
            assert!(!setting.is_default(&changed, &defaults));
            setting.restore(&mut changed, &defaults);
            assert!(setting.is_default(&changed, &defaults));
        }
        assert_eq!(changed, with(|s| s.show_names = true));
    }

    #[test]
    fn writes_are_brought_into_range() {
        let mut settings = Settings::default();
        TERRITORY_OPACITY.set(&mut settings, 4.0);
        WAR_OUTLINE_WIDTH.set(&mut settings, 9.0);
        LABEL_SIZE.set(&mut settings, 0.1);
        assert_eq!(settings.territory_fill_opacity, TERRITORY_OPACITY_MAX);
        assert_eq!(f64::from(settings.war_outline.width), WAR_OUTLINE_WIDTH_MAX);
        assert_eq!(settings.label_scale_master, LABEL_SCALE_MASTER_MIN);
    }

    #[test]
    fn the_heat_periods_are_kept_apart() {
        let mut settings = Settings::default();
        HEAT_LIVE_SEASON.set(&mut settings, true);
        assert_eq!(settings.heat_live_source, HeatLiveSource::Season);
        assert_eq!(
            settings.heat_history_basis,
            HeatHistoryBasis::SeasonCumulative
        );
        HEAT_HISTORY_SEASON.set(&mut settings, false);
        assert!(HEAT_LIVE_SEASON.get(&settings));
        assert!(!HEAT_HISTORY_SEASON.get(&settings));
    }
}
