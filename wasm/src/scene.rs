//! Which cached map layers have to be rebuilt for the next frame.
//!
//! The renderer keeps territory instances, connection lines, static labels (guild tags and
//! names), dynamic labels (timers) and icons in GPU buffers between frames. [`ScenePlanner`]
//! owns the decision of when each of them is stale, from what the host reports: data
//! changes, settings, the camera and the clock. Moving the camera without zooming rebuilds
//! nothing; the renderer only updates its projection.

use std::ops::BitOrAssign;

use crate::settings::RenderSettings;
use crate::viewport::Viewport;

/// Below this viewport scale per-territory timer text is hidden, keeping the overview clean.
pub const TIMER_VISIBILITY_MIN_SCALE: f64 = 0.31;
/// Below this viewport scale no territory labels or icons are drawn at all.
pub const LABEL_VISIBILITY_MIN_SCALE: f64 = 0.10;

/// Static labels are re-fitted every 1/320 of scale; finer steps would not be visible.
fn static_label_bucket(scale: f64) -> i32 {
    (scale * 320.0).floor() as i32
}

/// Connection lines are re-weighted every 1/20 of scale.
fn connection_bucket(scale: f64) -> i32 {
    (scale * 20.0).floor() as i32
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

/// A change in what the map shows, reported by the host side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneChange {
    /// Territory ownership, geometry, resources or runtime state.
    Territories,
    /// Hovered or selected territory.
    Highlight,
    /// Heat or war overlay data.
    Overlay,
    /// The icon atlas became available.
    Icons,
}

impl SceneChange {
    fn invalidates(self) -> Rebuild {
        match self {
            SceneChange::Territories => Rebuild::ALL,
            SceneChange::Highlight | SceneChange::Overlay => Rebuild {
                territories: true,
                ..Rebuild::NONE
            },
            SceneChange::Icons => Rebuild {
                icons: true,
                ..Rebuild::NONE
            },
        }
    }
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
    settings: Option<RenderSettings>,
    scale: Option<f64>,
    clock_secs: Option<i64>,
    labels_due_at: i64,
    icons_due_at: i64,
    /// Timer refreshes that fell due while the map was being manipulated.
    deferred: Rebuild,
}

impl Default for ScenePlanner {
    fn default() -> Self {
        Self {
            pending: Rebuild::ALL,
            settings: None,
            scale: None,
            clock_secs: None,
            labels_due_at: i64::MIN,
            icons_due_at: i64::MIN,
            deferred: Rebuild::NONE,
        }
    }
}

impl ScenePlanner {
    pub fn invalidate(&mut self, change: SceneChange) {
        self.pending |= change.invalidates();
    }

    /// The layers to rebuild for a frame drawn with this camera, settings and clock.
    /// `interacting` postpones timer-driven rebuilds until the gesture settles.
    pub fn plan(
        &mut self,
        camera: &Viewport,
        settings: &RenderSettings,
        clock_secs: i64,
        interacting: bool,
    ) -> Rebuild {
        let mut rebuild = std::mem::take(&mut self.pending);

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

fn scale_change(previous: f64, scale: f64) -> Rebuild {
    let timers_toggled =
        (previous >= TIMER_VISIBILITY_MIN_SCALE) != (scale >= TIMER_VISIBILITY_MIN_SCALE);
    Rebuild {
        territories: false,
        connections: connection_bucket(previous) != connection_bucket(scale),
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
            planner.plan(&at(scale), &settings(), clock, false),
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

    #[test]
    fn pure_translation_reuses_every_layer() {
        let mut planner = settled(0.5, 100, 200);
        for step in 0..60 {
            let mut camera = at(0.5);
            camera.pan(step as f64 * 7.0, step as f64 * -3.0);
            assert_eq!(planner.plan(&camera, &settings(), 100, true), Rebuild::NONE);
        }
    }

    #[test]
    fn zooming_refits_labels_by_scale_bucket_and_threshold() {
        let mut planner = settled(0.5, 100, 200);
        // Within one static bucket: only the pixel-thresholded layers.
        assert_eq!(
            planner.plan(&at(0.5001), &settings(), 100, true),
            only(|r| {
                r.dynamic_labels = true;
                r.icons = true;
            })
        );
        // Across a static bucket.
        assert!(
            planner
                .plan(&at(0.51), &settings(), 100, true)
                .static_labels
        );
        // Across a connection bucket.
        assert!(planner.plan(&at(0.56), &settings(), 100, true).connections);
        // Crossing the timer threshold within one bucket still refits static labels.
        let mut planner = settled(0.3101, 100, 200);
        let crossed = planner.plan(&at(0.3099), &settings(), 100, true);
        assert!(crossed.static_labels && crossed.dynamic_labels && crossed.icons);
        assert!(!crossed.territories);
    }

    #[test]
    fn clock_ticks_rebuild_timers_only_when_due() {
        let mut planner = settled(0.5, 100, 103);
        planner.schedule(NextRefresh {
            dynamic_labels: Some(103),
            icons: Some(160),
        });
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, false),
            Rebuild::NONE
        );
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 102, false),
            Rebuild::NONE
        );
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 103, false),
            only(|r| r.dynamic_labels = true)
        );
        planner.schedule(NextRefresh {
            dynamic_labels: Some(104),
            icons: None,
        });
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 160, false),
            only(|r| {
                r.dynamic_labels = true;
                r.icons = true;
            })
        );
    }

    #[test]
    fn timer_refreshes_wait_for_interaction_to_settle() {
        let mut planner = settled(0.5, 100, 101);
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, true),
            Rebuild::NONE
        );
        // Still interacting a frame later: nothing yet.
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, true),
            Rebuild::NONE
        );
        // Settled: the deferred refresh lands without needing another tick.
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, false),
            only(|r| {
                r.dynamic_labels = true;
                r.icons = true;
            })
        );
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, false),
            Rebuild::NONE
        );
    }

    #[test]
    fn a_zoom_during_interaction_absorbs_a_deferred_timer_refresh() {
        let mut planner = settled(0.5, 100, 101);
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 101, true),
            Rebuild::NONE
        );
        let zoomed = planner.plan(&at(0.52), &settings(), 101, true);
        assert!(zoomed.dynamic_labels && zoomed.icons);
        planner.schedule(NextRefresh {
            dynamic_labels: Some(102),
            icons: Some(500),
        });
        assert_eq!(
            planner.plan(&at(0.52), &settings(), 101, false),
            Rebuild::NONE
        );
    }

    #[test]
    fn repaints_at_a_paused_history_clock_reuse_every_layer() {
        // Animation-only repaints arrive every second while the history clock holds still.
        let mut planner = settled(0.5, 5_000, 5_001);
        for _ in 0..10 {
            assert_eq!(
                planner.plan(&at(0.5), &settings(), 5_000, false),
                Rebuild::NONE
            );
        }
        // Scrubbing the timeline moves the clock and refreshes the timers.
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 5_030, false),
            only(|r| {
                r.dynamic_labels = true;
                r.icons = true;
            })
        );
    }

    #[test]
    fn stepping_back_in_time_rebuilds_timers() {
        let mut planner = settled(0.5, 1_000, i64::MAX);
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 900, false),
            only(|r| {
                r.dynamic_labels = true;
                r.icons = true;
            })
        );
    }

    #[test]
    fn data_changes_invalidate_what_depends_on_them() {
        let mut planner = settled(0.5, 100, i64::MAX);
        planner.invalidate(SceneChange::Highlight);
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 100, true),
            only(|r| r.territories = true)
        );
        planner.invalidate(SceneChange::Overlay);
        planner.invalidate(SceneChange::Icons);
        assert_eq!(
            planner.plan(&at(0.5), &settings(), 100, true),
            only(|r| {
                r.territories = true;
                r.icons = true;
            })
        );
        planner.invalidate(SceneChange::Territories);
        assert_eq!(planner.plan(&at(0.5), &settings(), 100, true), Rebuild::ALL);
    }

    #[test]
    fn settings_changes_go_through_the_invalidation_matrix() {
        let mut planner = settled(0.5, 100, i64::MAX);
        let mut bold = settings();
        bold.bold_connections = true;
        assert_eq!(
            planner.plan(&at(0.5), &bold, 100, true),
            only(|r| r.connections = true)
        );
        assert_eq!(planner.plan(&at(0.5), &bold, 100, true), Rebuild::NONE);
    }
}
