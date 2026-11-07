//! Controls every sidebar view shares: its head, disclosures, and bringing newly opened
//! content into view inside the sidebar.

use leptos::prelude::*;

/// The sidebar's scrolling body. Its `scroll-padding-top` leaves room for the sticky head.
pub(crate) const SCROLLER: &str = "[data-sidebar-scroll]";

/// Scrolls the sidebar just enough to show the bottom of `element`, never so far that its top
/// leaves the view and never the page. Nothing happens when its top is already out of view:
/// the reader has moved on, and the content is theirs to scroll to.
pub(crate) fn reveal(element: &web_sys::Element) {
    let Ok(Some(scroller)) = element.closest(SCROLLER) else {
        return;
    };
    let view = scroller.get_bounding_client_rect();
    let target = element.get_bounding_client_rect();
    let top = clear_top(&scroller);
    let hidden_below = target.bottom() - view.bottom();
    if hidden_below <= 0.0 || target.top() < top || target.top() > view.bottom() {
        return;
    }
    let delta = hidden_below.min(target.top() - top);
    scroller.set_scroll_top(scroller.scroll_top() + delta.ceil() as i32);
}

/// [`reveal`] after the browser has laid out what was just shown.
pub(crate) fn reveal_soon(element: web_sys::Element) {
    request_animation_frame(move || reveal(&element));
}

/// Where the scroller's content comes out from under the open view's sticky head, which a long
/// title can make taller.
pub(crate) fn clear_top(scroller: &web_sys::Element) -> f64 {
    let top = scroller.get_bounding_client_rect().top();
    scroller
        .query_selector(".sb-head")
        .ok()
        .flatten()
        .map_or(top, |head| {
            head.get_bounding_client_rect().bottom().max(top)
        })
}

/// A `<details>` toggle: one that opened brings what it reveals into view. Says whether it is
/// now open.
pub(crate) fn disclosure_toggled(event: &web_sys::Event) -> bool {
    let element: web_sys::HtmlDetailsElement = event_target(event);
    let opened = element.open();
    if opened {
        reveal_soon(element.into());
    }
    opened
}

/// A `<details>` whose open state is held by `open`, so it survives its parent being rebuilt.
/// Opening it brings what it reveals into view.
#[component]
pub(crate) fn Disclosure(
    open: RwSignal<bool>,
    summary: &'static str,
    #[prop(optional)] class: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <details
            class=format!("tcard-disclosure {class}")
            prop:open=move || open.get()
            on:toggle=move |event| open.set(disclosure_toggled(&event))
        >
            <summary>{summary}</summary>
            {children()}
        </details>
    }
}

#[component]
pub(crate) fn CloseIcon() -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="16" height="16" aria-hidden="true">
            <path d="M6.28 5.22a.75.75 0 00-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 101.06 1.06L10 11.06l3.72 3.72a.75.75 0 101.06-1.06L11.06 10l3.72-3.72a.75.75 0 00-1.06-1.06L10 8.94 6.28 5.22z" />
        </svg>
    }
}

/// A pixel map pin, drawn on whole pixels to sit with the Minecraft type: Locate and Show on map.
#[component]
pub(crate) fn MapPinIcon() -> impl IntoView {
    view! {
        <svg class="ui-pin" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 5 7" fill="currentColor" fill-rule="evenodd" shape-rendering="crispEdges" width="10" height="14" aria-hidden="true">
            <path d="M1 0h3v1h1v3h-1v1h-1v2h-1v-2h-1v-1h-1v-3h1zM2 2h1v1h-1z" />
        </svg>
    }
}

/// The head of every sidebar view, one row that stays in place while the view scrolls: Back,
/// the view's title, its own actions or status, and Close. Each view fills only what it has. The
/// row is one control tall, and taller only for a name long enough to take a second line.
#[component]
pub(crate) fn ViewHead(
    /// What Back does, and what it says it goes back to.
    #[prop(optional)]
    back: Option<(&'static str, Callback<()>)>,
    /// The view's heading: an `h2`, after its owner's swatch for a guild or territory.
    #[prop(into)]
    title: ViewFn,
    #[prop(optional)] close: Option<(&'static str, Callback<()>)>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="sb-head sb-bar">
            {back.map(|(title, back)| view! {
                <button type="button" class="ui-button sb-nav-back" title=title aria-label=title on:click=move |_| back.run(())>
                    // The face's own chevron, so it sits on the same pixels as the word.
                    <span aria-hidden="true">"<"</span>
                    <span>"Back"</span>
                </button>
            })}
            <div class="sb-head-title">{move || title.run()}</div>
            {children.map(|children| view! { <div class="sb-head-actions">{children()}</div> })}
            {close.map(|(title, close)| view! {
                <button type="button" class="ui-button ui-icon-button" title=title aria-label=title on:click=move |_| close.run(())>
                    <CloseIcon />
                </button>
            })}
        </div>
    }
}
