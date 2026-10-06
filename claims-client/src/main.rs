mod app;
mod claims;

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
