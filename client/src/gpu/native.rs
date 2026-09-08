//! Host-only stand-in for compiling and testing shared client code.

use crate::app::NameColor;
use crate::renderer::{FrameMetrics, InvalidationReason, RenderCapabilities, SceneSnapshot};
use crate::tiles::LoadedTile;

pub type RenderFrameInput<'a> = SceneSnapshot<'a>;

pub struct GpuRenderer {
    pub thick_cooldown_borders: bool,
    pub resource_highlight: bool,
    pub defense_highlight: bool,
    pub use_static_gpu_labels: bool,
    pub use_full_gpu_text: bool,
    pub static_show_names: bool,
    pub show_claim_labels: bool,
    pub show_far_zoom_territory_tags: bool,
    pub static_abbreviate_names: bool,
    pub static_name_color: NameColor,
    pub static_tag_color: NameColor,
    pub show_connections: bool,
    pub bold_connections: bool,
    pub connection_opacity_scale: f32,
    pub connection_thickness_scale: f32,
    pub connection_zoom_fade_start: f32,
    pub connection_zoom_fade_end: f32,
    pub suppress_cooldown_visuals: bool,
    pub fill_alpha_boost: f32,
    pub use_readable_font: bool,
    pub dynamic_show_countdown: bool,
    pub dynamic_show_granular_map_time: bool,
    pub dynamic_show_compound_map_time: bool,
    pub dynamic_show_resource_icons: bool,
    pub show_territory_ornaments: bool,
    pub label_scale_master: f32,
    pub label_scale_static_tag: f32,
    pub label_scale_static_name: f32,
    pub label_scale_dynamic: f32,
    pub label_scale_icons: f32,
    capabilities: RenderCapabilities,
    metrics: FrameMetrics,
}

#[allow(dead_code)]
impl GpuRenderer {
    pub async fn init(_canvas: web_sys::HtmlCanvasElement) -> Result<Self, String> {
        Err("not wasm".into())
    }
    pub fn mark_dirty(&mut self, _reason: InvalidationReason) {}
    pub fn capabilities(&self) -> RenderCapabilities {
        self.capabilities
    }
    pub fn frame_metrics(&self) -> FrameMetrics {
        self.metrics
    }
    pub fn rebuild_text_renderer(&mut self) {}
    pub fn resize(&mut self, _w: u32, _h: u32, _dpr: f32) {}
    pub fn upload_tiles(&mut self, _tiles: &[LoadedTile]) {}
    pub fn render(&mut self, frame: RenderFrameInput<'_>) -> bool {
        let _ = (
            frame.vp,
            frame.territories,
            frame.hovered,
            frame.selected,
            frame.tiles,
            frame.world_bounds,
            frame.now,
            frame.reference_time_secs,
            frame.interaction_active,
            frame.icons,
            frame.show_minimap,
            frame.history_mode,
            frame.heat_mode_enabled,
            frame.heat_entries,
            frame.heat_max_take_count,
            frame.territories_in_war,
        );
        false
    }
}
