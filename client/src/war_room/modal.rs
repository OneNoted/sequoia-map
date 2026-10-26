//! Keyboard focus for the war sheet and the player detail: both are modal, so focus moves in
//! when they open, stays inside while they are open, and goes back where it came from.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

const FOCUSABLE: &str = "button:not([disabled]), a[href], input:not([disabled]), \
                         [tabindex]:not([tabindex='-1'])";

/// The element holding focus, to hand it back to later.
pub(super) fn focused_element() -> Option<HtmlElement> {
    web_sys::window()?
        .document()?
        .active_element()?
        .dyn_into::<HtmlElement>()
        .ok()
}

/// Focuses `element` on the next frame, once a just-opened modal is displayed: an element
/// that is still `display: none` cannot take focus.
pub(super) fn focus_next_frame(element: HtmlElement) {
    request_animation_frame(move || {
        let _ = element.focus();
    });
}

/// Hands focus back to `opener` if it is still in the document; a row rebuilt or removed
/// meanwhile has nothing to return to.
pub(super) fn restore_focus(opener: Option<HtmlElement>) {
    if let Some(opener) = opener.filter(|opener| opener.is_connected()) {
        let _ = opener.focus();
    }
}

/// Keeps Tab and Shift+Tab inside `container`.
pub(super) fn trap_tab(event: &web_sys::KeyboardEvent, container: &HtmlElement) {
    let Ok(nodes) = container.query_selector_all(FOCUSABLE) else {
        return;
    };
    let focusable: Vec<HtmlElement> = (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into().ok())
        .collect();
    let (Some(first), Some(last)) = (focusable.first(), focusable.last()) else {
        event.prevent_default();
        return;
    };
    let active = focused_element();
    let at = |element: &HtmlElement| active.as_ref() == Some(element);
    let on_container = active.as_ref() == Some(container);
    if event.shift_key() && (at(first) || on_container) {
        event.prevent_default();
        let _ = last.focus();
    } else if !event.shift_key() && at(last) {
        event.prevent_default();
        let _ = first.focus();
    }
}
