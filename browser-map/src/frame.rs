//! What the renderer draws each frame and what it reports back.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sequoia_map_engine::map_markers::MapMarkers;
use sequoia_map_engine::minimap::MinimapLayout;
use sequoia_map_engine::scene::NextRefresh;
use sequoia_map_engine::settings::RenderSettings;
use sequoia_map_engine::territory::ClientTerritoryMap;
use sequoia_map_engine::viewport::Viewport;

use crate::icons::ResourceAtlas;
use crate::tiles::LoadedTile;

/// One frame's view of the map, borrowed from the map's signals.
pub struct Frame<'a> {
    pub camera: &'a Viewport,
    pub territories: &'a ClientTerritoryMap,
    pub hovered: Option<&'a str>,
    pub selected: Option<&'a str>,
    pub settings: &'a RenderSettings,
    /// Recolour territories by capture count instead of guild colour.
    pub heat: Option<HeatColors<'a>>,
    /// Territories with a war in progress, outlined in red.
    pub wars: Option<&'a HashSet<String>>,
    /// Bounding box of all territories; tiles far outside it are not drawn.
    pub territory_bounds: Option<(f64, f64, f64, f64)>,
    pub tiles: &'a [LoadedTile],
    pub icons: Option<&'a ResourceAtlas>,
    /// Map Intel markers over the map.
    pub markers: Option<&'a Arc<MapMarkers>>,
    pub minimap: Option<MinimapLayout>,
    /// Seconds the timers count against: now, or the history timestamp.
    pub clock_secs: i64,
    pub now_ms: f64,
}

#[derive(Clone, Copy)]
pub struct HeatColors<'a> {
    pub take_counts: &'a HashMap<String, u64>,
    pub max_take_count: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameOutcome {
    /// Territory colour transitions are still running.
    pub animating: bool,
    pub next_refresh: NextRefresh,
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
