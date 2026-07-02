#[path = "../../client/src/animation.rs"]
mod animation;
mod app;
#[path = "../../client/src/assets.rs"]
mod assets;
#[path = "../../client/src/canvas.rs"]
mod canvas;
#[path = "../../client/src/claim_labels.rs"]
mod claim_labels;
#[path = "../../client/src/claims.rs"]
mod claims;
#[path = "../../client/src/colors.rs"]
mod colors;
#[path = "../../client/src/defense.rs"]
mod defense;
#[cfg(target_arch = "wasm32")]
#[path = "../../client/src/gpu/mod.rs"]
mod gpu;
#[path = "../../client/src/heat.rs"]
mod heat;
mod history;
#[path = "../../client/src/icons.rs"]
mod icons;
#[path = "../../client/src/label_layout.rs"]
mod label_layout;
#[path = "../../client/src/overlay_sizing.rs"]
mod overlay_sizing;
#[path = "../../client/src/render_loop.rs"]
mod render_loop;
#[path = "../../client/src/renderer/mod.rs"]
mod renderer;
#[path = "../../client/src/spatial.rs"]
mod spatial;
#[path = "../../client/src/sse.rs"]
mod sse;
#[path = "../../client/src/territory.rs"]
mod territory;
#[path = "../../client/src/tiles.rs"]
mod tiles;
#[path = "../../client/src/time_format.rs"]
mod time_format;
#[path = "../../client/src/viewport.rs"]
mod viewport;

#[cfg(not(target_arch = "wasm32"))]
mod gpu {
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
        pub fn mark_instance_dirty(&mut self) {}
        pub fn mark_text_dirty(&mut self) {}
        pub fn mark_dynamic_text_dirty(&mut self) {}
        pub fn mark_icon_dirty(&mut self) {}
        pub fn mark_connection_dirty(&mut self) {}
        pub fn mark_dirty(&mut self, _reason: InvalidationReason) {}
        pub fn capabilities(&self) -> RenderCapabilities {
            self.capabilities
        }
        pub fn frame_metrics(&self) -> FrameMetrics {
            self.metrics
        }
        pub fn supports_static_gpu_labels(&self) -> bool {
            false
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
            );
            false
        }
    }
}

use leptos::mount::mount_to;
use std::any::Any;
use std::cell::RefCell;
use wasm_bindgen::JsCast;

thread_local! {
    static APP_MOUNT_HANDLE: RefCell<Option<Box<dyn Any>>> = RefCell::new(None);
}

fn main() {
    console_error_panic_hook::set_once();
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let mount_target = document
        .get_element_by_id("app")
        .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .or_else(|| document.body());
    let Some(target) = mount_target else {
        return;
    };

    APP_MOUNT_HANDLE.with(move |slot| {
        let _old = slot.borrow_mut().take();
        let handle = mount_to(target, app::App);
        *slot.borrow_mut() = Some(Box::new(handle));
    });
}
