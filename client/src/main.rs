mod animation;
mod app;
mod canvas;
mod claim_labels;
mod colors;
#[cfg(target_arch = "wasm32")]
mod gpu;
mod heat;
mod history;
mod icons;
mod label_layout;
mod overlay_sizing;
mod playback;
mod render_loop;
mod renderer;
mod season_scalar;
mod sidebar;
mod spatial;
mod sse;
mod territory;
mod tiles;
mod time_format;
mod timeline;
mod tower;
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
        pub use_static_gpu_labels: bool,
        pub use_full_gpu_text: bool,
        pub static_show_names: bool,
        pub static_abbreviate_names: bool,
        pub static_name_color: NameColor,
        pub static_tag_color: NameColor,
        pub show_connections: bool,
        pub bold_connections: bool,
        pub connection_opacity_scale: f32,
        pub connection_thickness_scale: f32,
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
use leptos::prelude::*;
use leptos_router::components::Router;
use std::any::Any;
use std::cell::RefCell;
use wasm_bindgen::JsCast;

thread_local! {
    static APP_MOUNT_HANDLE: RefCell<Option<Box<dyn Any>>> = RefCell::new(None);
}

pub(crate) const SEQUOIA_WEBSITE_URL: &str = "https://seqwawa.com";
pub(crate) const IRIS_RELEASES_URL: &str = "https://github.com/OneNoted/sequoia-map/releases";

fn encode_uri_component_fallback(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        let is_safe = matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
        );
        if is_safe {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    encoded
}

#[cfg(target_arch = "wasm32")]
fn encode_guild_name_for_url(guild_name: &str) -> String {
    js_sys::encode_uri_component(guild_name)
        .as_string()
        .unwrap_or_else(|| encode_uri_component_fallback(guild_name))
}

#[cfg(not(target_arch = "wasm32"))]
fn encode_guild_name_for_url(guild_name: &str) -> String {
    encode_uri_component_fallback(guild_name)
}

pub(crate) fn guild_stats_url(guild_name: &str) -> String {
    let encoded = encode_guild_name_for_url(guild_name);
    format!("https://wynncraft.com/stats/guild/{encoded}")
}

#[cfg(test)]
mod tests {
    use super::{encode_uri_component_fallback, guild_stats_url};

    #[test]
    fn fallback_uri_encoder_escapes_reserved_characters() {
        assert_eq!(encode_uri_component_fallback("A/B? C"), "A%2FB%3F%20C");
    }

    #[test]
    fn guild_stats_url_uses_encoded_path_segment() {
        assert_eq!(
            guild_stats_url("Sequoia/Map? Guild"),
            "https://wynncraft.com/stats/guild/Sequoia%2FMap%3F%20Guild"
        );
    }
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
        // If main() is re-entered (e.g. dev/hot-reload runtime quirks), drop the old mount
        // so stale effects/signals can't keep mutating app state.
        let _old = slot.borrow_mut().take();
        let handle = mount_to(target, || {
            view! {
                <Router>
                    <app::App />
                </Router>
            }
        });
        *slot.borrow_mut() = Some(Box::new(handle));
    });
}
