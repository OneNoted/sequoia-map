//! Which cached map layers have to be rebuilt for the next frame.
//!
//! The renderer keeps territory instances, connection lines, static labels (guild tags and
//! names), dynamic labels (timers) and icons in GPU buffers between frames. [`ScenePlanner`]
//! owns the decision of when each of them is stale, from what the host reports: data
//! changes, settings, the camera and the clock. Moving the camera without zooming rebuilds
//! nothing while the view stays inside the area the labels were laid out for; the renderer
//! only updates its projection.
//!
//! Labels and icons are laid out only for the territories in that area, [`ScenePlanner::
//! coverage`]: the view with a view's width and height of margin around it. It is laid out
//! afresh when the view leaves it, or has shrunk to a small part of it by zooming in, so
//! a zoom costs what is near the view rather than the whole map.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::BitOrAssign;

use crate::settings::{ConnectionStyle, RenderSettings};
use crate::territory::ClientTerritoryMap;
use crate::viewport::{Viewport, WorldRect};

/// Below this viewport scale per-territory timer text is hidden, keeping the overview clean.
pub const TIMER_VISIBILITY_MIN_SCALE: f64 = 0.31;
/// Below this viewport scale no territory labels or icons are drawn at all.
pub const LABEL_VISIBILITY_MIN_SCALE: f64 = 0.10;

/// The laid-out area is laid out afresh once the view covers less than this share of it.
const COVERAGE_MIN_SHARE: f64 = 1.0 / 25.0;

/// Static labels are re-fitted every 1/320 of scale; finer steps would not be visible.
fn static_label_bucket(scale: f64) -> i32 {
    (scale * 320.0).floor() as i32
}

/// The cached layers the renderer rebuilds before drawing a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rebuild {
    /// Territory fills, borders and highlight state.
    pub territories: bool,
    pub connections: bool,
    /// Guild tags, territory names and claim labels.
    pub static_labels: bool,
    /// Held-time and cooldown timers.
    pub dynamic_labels: bool,
    /// Resource icons, HQ crowns and ornaments.
    pub icons: bool,
}

impl Rebuild {
    pub const NONE: Self = Self {
        territories: false,
        connections: false,
        static_labels: false,
        dynamic_labels: false,
        icons: false,
    };
    pub const ALL: Self = Self {
        territories: true,
        connections: true,
        static_labels: true,
        dynamic_labels: true,
        icons: true,
    };

    pub fn any(&self) -> bool {
        *self != Self::NONE
    }
}

impl BitOrAssign for Rebuild {
    fn bitor_assign(&mut self, other: Self) {
        self.territories |= other.territories;
        self.connections |= other.connections;
        self.static_labels |= other.static_labels;
        self.dynamic_labels |= other.dynamic_labels;
        self.icons |= other.icons;
    }
}

/// What a new territory map changed of what the renderer draws, against the last one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerritoryChange {
    /// Territories came or went, moved, or changed their connections or resources.
    pub geometry: bool,
    /// Owners, their colours or acquisition times changed.
    pub ownership: bool,
    /// Headquarters or defence tiers changed.
    pub details: bool,
}

impl TerritoryChange {
    pub fn any(&self) -> bool {
        self.geometry || self.ownership || self.details
    }
}

/// Fingerprints, by aspect, of what the renderer draws from a territory map, so a new map can
/// be told apart from the last one without keeping a copy of it. Runtime data the renderer
/// does not draw (treasury, stored resources, war state) is not part of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerritoryDigest {
    geometry: u64,
    ownership: u64,
    details: u64,
}

impl TerritoryDigest {
    pub fn of(territories: &ClientTerritoryMap) -> Self {
        // Each territory hashes with its name and the sums ignore map order.
        let hash = |name: &str, fields: &dyn Fn(&mut DefaultHasher)| {
            let mut hasher = DefaultHasher::new();
            name.hash(&mut hasher);
            fields(&mut hasher);
            hasher.finish()
        };
        let mut digest = Self::default();
        for (name, ct) in territories {
            let territory = &ct.territory;
            digest.geometry = digest.geometry.wrapping_add(hash(name, &|h| {
                (territory.location.start, territory.location.end).hash(h);
                territory.connections.hash(h);
                let r = &territory.resources;
                (r.emeralds, r.ore, r.crops, r.fish, r.wood).hash(h);
            }));
            digest.ownership = digest.ownership.wrapping_add(hash(name, &|h| {
                let guild = &territory.guild;
                (&guild.uuid, &guild.name, &guild.prefix, guild.color).hash(h);
                ct.guild_color.hash(h);
                territory.acquired.timestamp_millis().hash(h);
                ct.animation
                    .as_ref()
                    .map(|transition| transition.start_time.to_bits())
                    .hash(h);
            }));
            digest.details = digest.details.wrapping_add(hash(name, &|h| {
                let runtime = territory.runtime.as_ref();
                runtime.and_then(|runtime| runtime.headquarters).hash(h);
                runtime
                    .and_then(|runtime| runtime.defense_tier.as_deref())
                    .hash(h);
            }));
        }
        digest
    }

    /// What changed since `previous`; everything when there was none.
    pub fn since(&self, previous: Option<&Self>) -> TerritoryChange {
        let changed = |aspect: fn(&Self) -> u64| previous.is_none_or(|p| aspect(p) != aspect(self));
        TerritoryChange {
            geometry: changed(|d| d.geometry),
            ownership: changed(|d| d.ownership),
            details: changed(|d| d.details),
        }
    }
}

/// A change in what the map shows, reported by the host side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneChange {
    Territories(TerritoryChange),
    /// Heat or war overlay data.
    Overlay,
    /// The icon atlas became available.
    Icons,
}

/// When the timer-dependent layers built this frame next change, in clock seconds.
/// `None` for a layer that was not rebuilt; `i64::MAX` for one that never changes by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NextRefresh {
    pub dynamic_labels: Option<i64>,
    pub icons: Option<i64>,
}

/// Decides, frame by frame, which cached layers the renderer must rebuild.
#[derive(Debug)]
pub struct ScenePlanner {
    pending: Rebuild,
    /// Owners changed since the last frame: connections follow when they are drawn in guild
    /// colours, which the settings at that frame decide.
    ownership_changed: bool,
    settings: Option<RenderSettings>,
    scale: Option<f64>,
    clock_secs: Option<i64>,
    labels_due_at: i64,
    icons_due_at: i64,
    /// Timer refreshes that fell due while the map was being manipulated.
    deferred: Rebuild,
    coverage: Option<WorldRect>,
}

impl Default for ScenePlanner {
    fn default() -> Self {
        Self {
            pending: Rebuild::ALL,
            ownership_changed: false,
            settings: None,
            scale: None,
            clock_secs: None,
            labels_due_at: i64::MIN,
            icons_due_at: i64::MIN,
            deferred: Rebuild::NONE,
            coverage: None,
        }
    }
}

impl ScenePlanner {
    pub fn invalidate(&mut self, change: SceneChange) {
        self.pending |= match change {
            SceneChange::Territories(change) if change.geometry => Rebuild::ALL,
            SceneChange::Territories(change) if change.ownership || change.details => {
                self.ownership_changed |= change.ownership;
                Rebuild {
                    connections: false,
                    ..Rebuild::ALL
                }
            }
            SceneChange::Territories(_) => Rebuild::NONE,
            SceneChange::Overlay => Rebuild {
                territories: true,
                ..Rebuild::NONE
            },
            SceneChange::Icons => Rebuild {
                icons: true,
                ..Rebuild::NONE
            },
        };
    }

    /// The layers to rebuild for a frame drawn with this camera on a `view` of that many CSS
    /// pixels, these settings and clock. `interacting` postpones timer-driven rebuilds until
    /// the gesture settles.
    pub fn plan(
        &mut self,
        camera: &Viewport,
        view: (f64, f64),
        settings: &RenderSettings,
        clock_secs: i64,
        interacting: bool,
    ) -> Rebuild {
        let mut rebuild = std::mem::take(&mut self.pending);
        let visible = camera.visible(view.0, view.1);
        let covered = self.coverage.is_some_and(|coverage| {
            coverage.contains(&visible)
                && visible.width() * visible.height()
                    >= coverage.width() * coverage.height() * COVERAGE_MIN_SHARE
        });
        if !covered {
            self.coverage = Some(visible.grown(visible.width(), visible.height()));
            rebuild.static_labels = true;
            rebuild.dynamic_labels = true;
            rebuild.icons = true;
        }
        if std::mem::take(&mut self.ownership_changed) && connections_follow_owners(settings) {
            rebuild.connections = true;
        }

        match &self.settings {
            Some(previous) if previous == settings => {}
            previous => {
                rebuild |= previous
                    .as_ref()
                    .map_or(Rebuild::ALL, |previous| settings.invalidates(previous));
                self.settings = Some(settings.clone());
            }
        }

        match self.scale.replace(camera.scale) {
            Some(previous) if previous == camera.scale => {}
            Some(previous) => rebuild |= scale_change(previous, camera.scale),
            None => rebuild = Rebuild::ALL,
        }

        if self.clock_secs != Some(clock_secs) {
            let stepped_back = self
                .clock_secs
                .is_some_and(|previous| clock_secs < previous);
            self.deferred |= Rebuild {
                dynamic_labels: stepped_back || clock_secs >= self.labels_due_at,
                icons: stepped_back || clock_secs >= self.icons_due_at,
                ..Rebuild::NONE
            };
            self.clock_secs = Some(clock_secs);
        }
        if !interacting {
            rebuild |= std::mem::take(&mut self.deferred);
        }
        // Whatever is rebuilt now catches up with the clock as well.
        self.deferred.dynamic_labels &= !rebuild.dynamic_labels;
        self.deferred.icons &= !rebuild.icons;
        rebuild
    }

    /// The world area the label and icon layers lay out territories in; nothing outside it
    /// can be in view until the planner lays them out again.
    pub fn coverage(&self) -> Option<WorldRect> {
        self.coverage
    }

    /// Records when the layers just rebuilt next change on their own.
    pub fn schedule(&mut self, next: NextRefresh) {
        if let Some(at) = next.dynamic_labels {
            self.labels_due_at = at;
        }
        if let Some(at) = next.icons {
            self.icons_due_at = at;
        }
    }
}

/// Whether connection lines are drawn in guild colours.
fn connections_follow_owners(settings: &RenderSettings) -> bool {
    match settings.connection_style {
        ConnectionStyle::Guild => true,
        ConnectionStyle::Classic => settings.bold_connections,
        ConnectionStyle::White => false,
    }
}

fn scale_change(previous: f64, scale: f64) -> Rebuild {
    let timers_toggled =
        (previous >= TIMER_VISIBILITY_MIN_SCALE) != (scale >= TIMER_VISIBILITY_MIN_SCALE);
    Rebuild {
        territories: false,
        // Connection vertices do not depend on the camera; the renderer applies the classic
        // spacing and fade for the exact scale when it draws them.
        connections: false,
        static_labels: timers_toggled
            || static_label_bucket(previous) != static_label_bucket(scale),
        // Timer text and icons honour on-screen pixel minimums at the exact scale.
        dynamic_labels: true,
        icons: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::settings;
    use crate::territory::ClientTerritory;

    const VIEW: (f64, f64) = (1000.0, 800.0);

    fn at(scale: f64) -> Viewport {
        Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale,
        }
    }

    /// A planner that has drawn one frame at `scale` and `clock`, with timers due at `due`.
    fn settled(scale: f64, clock: i64, due: i64) -> ScenePlanner {
        let mut planner = ScenePlanner::default();
        assert_eq!(
            planner.plan(&at(scale), VIEW, &settings(), clock, false),
            Rebuild::ALL
        );
        planner.schedule(NextRefresh {
            dynamic_labels: Some(due),
            icons: Some(due),
        });
        planner
    }

    fn only(change: impl FnOnce(&mut Rebuild)) -> Rebuild {
        let mut rebuild = Rebuild::NONE;
        change(&mut rebuild);
        rebuild
    }

    fn timers() -> Rebuild {
        only(|r| {
            r.dynamic_labels = true;
            r.icons = true;
        })
    }

    fn territory(guild: &str, x: i32) -> ClientTerritory {
        let territory = serde_json::from_value(serde_json::json!({
            "guild": {"uuid": guild, "name": guild, "prefix": "TST"},
            "acquired": "2026-10-11T00:00:00Z",
            "location": {"start": [x, 0], "end": [x + 100, 100]},
            "connections": ["B"],
            "runtime": {"headquarters": false, "treasury": "Low", "defense_tier": "Low"},
        }))
        .expect("territory json");
        ClientTerritory::from_territory("A", territory)
    }

    fn map(territory: ClientTerritory) -> ClientTerritoryMap {
        ClientTerritoryMap::from([("A".to_string(), territory)])
    }

    /// What a planner that drew `before` rebuilds after the map becomes `after`.
    fn rebuild_after(
        before: &ClientTerritoryMap,
        after: &ClientTerritoryMap,
        settings: &RenderSettings,
    ) -> Rebuild {
        let mut planner = ScenePlanner::default();
        planner.plan(&at(0.5), VIEW, settings, 100, false);
        let change = TerritoryDigest::of(after).since(Some(&TerritoryDigest::of(before)));
        planner.invalidate(SceneChange::Territories(change));
        planner.plan(&at(0.5), VIEW, settings, 100, false)
    }

    #[test]
    fn pure_translation_reuses_every_layer() {
        let mut planner = settled(0.5, 100, 200);
        for step in 0..60 {
            let mut camera = at(0.5);
            camera.pan(step as f64 * 7.0, step as f64 * -3.0);
            assert_eq!(
                planner.plan(&camera, VIEW, &settings(), 100, true),
                Rebuild::NONE
            );
        }
    }

    #[test]
    fn zooming_never_rebuilds_territories_or_connections_in_any_style() {
        let mut solid = settings();
        solid.connection_style = crate::settings::ConnectionStyle::White;
        for style in [settings(), solid] {
            let mut planner = ScenePlanner::default();
            assert_eq!(
                planner.plan(&at(0.25), VIEW, &style, 100, false),
                Rebuild::ALL
            );
            for scale in [0.263, 0.277, 0.6, 2.0] {
                let rebuild = planner.plan(&at(scale), VIEW, &style, 100, true);
                assert!(!rebuild.territories && !rebuild.connections, "{scale}");
                assert!(rebuild.dynamic_labels, "{scale}");
            }
        }
    }

    #[test]
    fn crossing_the_timer_threshold_refits_static_labels() {
        let mut planner = settled(TIMER_VISIBILITY_MIN_SCALE + 1e-4, 100, 200);
        let crossed = planner.plan(
            &at(TIMER_VISIBILITY_MIN_SCALE - 1e-4),
            VIEW,
            &settings(),
            100,
            true,
        );
        assert!(crossed.static_labels && crossed.dynamic_labels && crossed.icons);
    }

    #[test]
    fn clock_ticks_rebuild_timers_only_when_due() {
        let mut planner = settled(0.5, 100, 103);
        planner.schedule(NextRefresh {
            dynamic_labels: Some(103),
            icons: Some(160),
        });
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 101, false),
            Rebuild::NONE
        );
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 102, false),
            Rebuild::NONE
        );
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 103, false),
            only(|r| r.dynamic_labels = true)
        );
        planner.schedule(NextRefresh {
            dynamic_labels: Some(104),
            icons: None,
        });
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 160, false),
            timers()
        );
    }

    #[test]
    fn timer_refreshes_land_once_the_map_settles() {
        let mut planner = settled(0.5, 100, 101);
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 101, true),
            Rebuild::NONE
        );
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 101, false),
            timers()
        );
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 101, false),
            Rebuild::NONE
        );
    }

    #[test]
    fn repaints_at_a_paused_history_clock_reuse_every_layer() {
        // Animation-only repaints arrive every second while the history clock holds still.
        let mut planner = settled(0.5, 5_000, 5_001);
        for _ in 0..10 {
            assert_eq!(
                planner.plan(&at(0.5), VIEW, &settings(), 5_000, false),
                Rebuild::NONE
            );
        }
        // Scrubbing the timeline moves the clock and refreshes the timers.
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 5_030, false),
            timers()
        );
    }

    #[test]
    fn stepping_back_in_time_rebuilds_timers() {
        let mut planner = settled(0.5, 1_000, i64::MAX);
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 900, false),
            timers()
        );
    }

    #[test]
    fn a_live_update_of_undrawn_runtime_data_rebuilds_nothing() {
        let before = map(territory("One", 0));
        let mut after = before.clone();
        let runtime = after
            .get_mut("A")
            .unwrap()
            .territory
            .runtime
            .as_mut()
            .unwrap();
        runtime.treasury = Some("Very High".into());
        assert_eq!(rebuild_after(&before, &after, &settings()), Rebuild::NONE);
    }

    #[test]
    fn a_new_headquarters_rebuilds_everything_but_connections() {
        let before = map(territory("One", 0));
        let mut after = before.clone();
        let runtime = after
            .get_mut("A")
            .unwrap()
            .territory
            .runtime
            .as_mut()
            .unwrap();
        runtime.headquarters = Some(true);
        let rebuild = rebuild_after(&before, &after, &settings());
        assert_eq!(
            rebuild,
            Rebuild {
                connections: false,
                ..Rebuild::ALL
            }
        );
    }

    #[test]
    fn new_owners_redraw_connections_only_where_they_carry_guild_colours() {
        let before = map(territory("One", 0));
        let after = map(territory("Two", 0));
        assert!(!rebuild_after(&before, &after, &settings()).connections);
        let mut tinted = settings();
        tinted.bold_connections = true;
        assert_eq!(rebuild_after(&before, &after, &tinted), Rebuild::ALL);
        let mut guild = settings();
        guild.connection_style = crate::settings::ConnectionStyle::Guild;
        assert_eq!(rebuild_after(&before, &after, &guild), Rebuild::ALL);
    }

    #[test]
    fn moved_or_reconnected_territories_rebuild_every_layer() {
        let before = map(territory("One", 0));
        assert_eq!(
            rebuild_after(&before, &map(territory("One", 50)), &settings()),
            Rebuild::ALL
        );
        let mut reconnected = before.clone();
        reconnected
            .get_mut("A")
            .unwrap()
            .territory
            .connections
            .clear();
        assert_eq!(
            rebuild_after(&before, &reconnected, &settings()),
            Rebuild::ALL
        );
    }

    #[test]
    fn overlay_and_icon_changes_rebuild_their_layers() {
        let mut planner = settled(0.5, 100, i64::MAX);
        planner.invalidate(SceneChange::Overlay);
        planner.invalidate(SceneChange::Icons);
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &settings(), 100, true),
            only(|r| {
                r.territories = true;
                r.icons = true;
            })
        );
    }

    #[test]
    fn settings_changes_go_through_the_invalidation_matrix() {
        let mut planner = settled(0.5, 100, i64::MAX);
        let mut bold = settings();
        bold.bold_connections = true;
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &bold, 100, true),
            only(|r| r.connections = true)
        );
        assert_eq!(
            planner.plan(&at(0.5), VIEW, &bold, 100, true),
            Rebuild::NONE
        );
    }
}
