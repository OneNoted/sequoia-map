//! Host-only stand-in for the wgpu renderer, so the map crate builds and tests natively.
//! It can never be initialised, so nothing is drawn.

use sequoia_map_engine::scene::Rebuild;

use crate::frame::{Frame, FrameMetrics, FrameOutcome, RenderCapabilities};

// The renderer's label fitting needs no GPU, so its tests run here; nothing else uses it.
#[cfg(test)]
#[allow(dead_code)]
mod glyphs;

pub enum GpuRenderer {}

impl GpuRenderer {
    pub async fn init(_canvas: web_sys::HtmlCanvasElement) -> Result<Self, String> {
        Err("the map renderer needs WebGL2 in a browser".into())
    }

    pub fn capabilities(&self) -> RenderCapabilities {
        match *self {}
    }

    pub fn frame_metrics(&self) -> FrameMetrics {
        match *self {}
    }

    pub fn max_surface_side(&self) -> u32 {
        match *self {}
    }

    pub fn resize(&mut self, _width: u32, _height: u32, _dpr: f32) {
        match *self {}
    }

    pub fn pending_text_font(&self) -> Option<String> {
        match *self {}
    }

    pub fn text_font_loaded(&mut self, _font: &str) {
        match *self {}
    }

    pub fn render(&mut self, frame: &Frame, rebuild: Rebuild) -> FrameOutcome {
        // Everything the browser renderer reads, so both builds agree on what is used.
        let _ = (
            frame.camera,
            frame.territories,
            frame.territory_revision,
            frame.hovered,
            frame.selected,
            frame.settings,
            frame.heat.map(|heat| (heat.take_counts, heat.scale)),
            frame.war_stages,
            frame.territory_bounds,
            frame.tiles,
            frame.icons,
            frame.markers,
            frame.site_labels,
            frame.war_plates,
            frame.teammates,
            frame.minimap,
            frame.clock_secs,
            frame.now_ms,
            rebuild,
        );
        match *self {}
    }
}
