use std::collections::{HashMap, HashSet};

use crate::icons::ResourceAtlas;
use crate::territory::ClientTerritoryMap;
use crate::tiles::LoadedTile;
use crate::viewport::Viewport;

/// Explicit reasons for invalidating renderer-side caches/buffers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InvalidationReason {
    Geometry,
    StaticLabel,
    DynamicLabel,
    Viewport,
    Resources,
}

/// Runtime renderer capabilities resolved at initialization.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct RenderCapabilities {
    pub webgl2: bool,
    pub gpu_text_msdf: bool,
    pub gpu_dynamic_labels: bool,
    pub compatibility_fallback: bool,
}

/// Per-frame metrics for perf instrumentation and telemetry.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct FrameMetrics {
    pub frame_cpu_ms: f64,
    pub draw_calls: u32,
    pub tile_draw_calls: u32,
    pub bytes_uploaded: u64,
    pub resolution_scale: f32,
    pub territory_instances: u32,
    pub text_instances: u32,
    pub fps_estimate: f64,
}

/// Immutable, frame-local scene input snapshot.
#[derive(Clone, Copy)]
pub struct SceneSnapshot<'a> {
    pub vp: &'a Viewport,
    pub territories: &'a ClientTerritoryMap,
    pub hovered: &'a Option<String>,
    pub selected: &'a Option<String>,
    pub tiles: &'a [LoadedTile],
    pub world_bounds: Option<(f64, f64, f64, f64)>,
    pub now: f64,
    pub reference_time_secs: i64,
    pub interaction_active: bool,
    pub icons: &'a Option<ResourceAtlas>,
    pub show_minimap: bool,
    pub history_mode: bool,
    pub heat_mode_enabled: bool,
    pub heat_entries: &'a HashMap<String, u64>,
    pub heat_max_take_count: u64,
    /// Names of the territories with a war in progress; drives the red territory border.
    pub territories_in_war: &'a HashSet<String>,
}

/// Lightweight metadata emitted alongside each built scene snapshot.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SceneSummary {
    pub territory_count: usize,
    pub tile_count: usize,
    pub has_hovered: bool,
    pub has_selected: bool,
    pub interaction_active: bool,
    pub show_minimap: bool,
    pub history_mode: bool,
    pub reference_time_secs: i64,
    pub heat_mode_enabled: bool,
    pub heat_max_take_count: u64,
}

impl<'a> From<&SceneSnapshot<'a>> for SceneSummary {
    fn from(snapshot: &SceneSnapshot<'a>) -> Self {
        Self {
            territory_count: snapshot.territories.len(),
            tile_count: snapshot.tiles.len(),
            has_hovered: snapshot.hovered.is_some(),
            has_selected: snapshot.selected.is_some(),
            interaction_active: snapshot.interaction_active,
            show_minimap: snapshot.show_minimap,
            history_mode: snapshot.history_mode,
            reference_time_secs: snapshot.reference_time_secs,
            heat_mode_enabled: snapshot.heat_mode_enabled,
            heat_max_take_count: snapshot.heat_max_take_count,
        }
    }
}

/// Double-buffered scene builder to avoid tearing between preparation and render.
#[derive(Clone, Debug, Default)]
pub struct SceneBuilder {
    front: SceneSummary,
    back: SceneSummary,
}

impl SceneBuilder {
    pub fn build<'a>(&mut self, snapshot: SceneSnapshot<'a>) -> SceneSnapshot<'a> {
        self.back = SceneSummary::from(&snapshot);
        std::mem::swap(&mut self.front, &mut self.back);
        snapshot
    }

    pub fn latest_summary(&self) -> SceneSummary {
        self.front
    }
}
