mod app;
#[path = "../../client/src/assets.rs"]
mod assets;
#[path = "../../client/src/canvas.rs"]
mod canvas;
#[path = "../../client/src/claims.rs"]
mod claims;
#[cfg(target_arch = "wasm32")]
#[path = "../../client/src/gpu/mod.rs"]
mod gpu;
mod history;
#[path = "../../client/src/icons.rs"]
mod icons;
#[path = "../../client/src/render_loop.rs"]
mod render_loop;
#[path = "../../client/src/renderer/mod.rs"]
mod renderer;
#[path = "../../client/src/sse.rs"]
mod sse;
#[path = "../../client/src/tiles.rs"]
mod tiles;

// Shared map math and state helpers, also used by the other browser client.
#[cfg(target_arch = "wasm32")]
pub(crate) use sequoia_map_engine::{claim_labels, label_layout, overlay_sizing};
#[cfg(target_arch = "wasm32")]
pub(crate) use sequoia_map_engine::{colors, defense, time_format};
pub(crate) use sequoia_map_engine::{spatial, territory, viewport};

#[cfg(not(target_arch = "wasm32"))]
#[path = "../../client/src/gpu/native.rs"]
mod gpu;

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
