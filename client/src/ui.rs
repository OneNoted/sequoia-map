//! Controls every sidebar view shares: its navigation row, disclosures, and bringing newly
//! opened content into view inside the sidebar.

use leptos::prelude::*;

/// The sidebar's scrolling body. Its `scroll-padding-top` leaves room for the sticky nav row.
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
    let top = view.top() + scroll_padding_top(&scroller);
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

fn scroll_padding_top(element: &web_sys::Element) -> f64 {
    web_sys::window()
        .and_then(|window| window.get_computed_style(element).ok().flatten())
        .and_then(|style| style.get_property_value("scroll-padding-top").ok())
        .and_then(|value| value.trim_end_matches("px").parse().ok())
        .unwrap_or(0.0)
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
            on:toggle=move |event| {
                let element: web_sys::HtmlDetailsElement = event_target(&event);
                let opened = element.open();
                open.set(opened);
                if opened {
                    reveal_soon(element.into());
                }
            }
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

/// A target: what Show on map centres the map on.
#[component]
pub(crate) fn CrosshairIcon() -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" width="16" height="16" aria-hidden="true">
            <circle cx="10" cy="10" r="5.5" />
            <path d="M10 1.75v3M10 15.25v3M1.75 10h3M15.25 10h3" />
            <circle cx="10" cy="10" r="1.5" fill="currentColor" stroke="none" />
        </svg>
    }
}

/// The row at the top of every sidebar view: Back on the left, Close on the right, and the
/// view's own actions or title between. It stays in place while the view scrolls.
#[component]
pub(crate) fn PanelNav(
    /// What Back does, and what it says it goes back to.
    #[prop(optional)]
    back: Option<(&'static str, Callback<()>)>,
    #[prop(optional)] close: Option<(&'static str, Callback<()>)>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="sb-nav sb-bar">
            {back.map(|(title, back)| view! {
                <button type="button" class="ui-button sb-nav-back" title=title aria-label=title on:click=move |_| back.run(())>
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="14" height="14" aria-hidden="true">
                        <path fill-rule="evenodd" d="M12.79 5.23a.75.75 0 01-.02 1.06L8.83 10l3.94 3.71a.75.75 0 11-1.04 1.08l-4.5-4.25a.75.75 0 010-1.08l4.5-4.25a.75.75 0 011.06.02z" clip-rule="evenodd" />
                    </svg>
                    <span>"Back"</span>
                </button>
            })}
            <div class="sb-nav-middle">{children.map(|children| children())}</div>
            {close.map(|(title, close)| view! {
                <button type="button" class="ui-button ui-icon-button" title=title aria-label=title on:click=move |_| close.run(())>
                    <CloseIcon />
                </button>
            })}
        </div>
    }
}
