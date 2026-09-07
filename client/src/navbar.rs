//! The seqwawa.com site navbar, floating over the map canvas.
//!
//! A port of `sequoia-web/frontend/src/lib/components/Navbar.svelte`, kept
//! structurally identical to it so that crossing between the two sites is
//! seamless - same bar, same menu, same breakpoint. The bar is
//! `position: fixed` and translucent, so the canvas stays full-bleed underneath
//! it and no map area is lost. It carries the site sections, the Discord link,
//! and the account control - and nothing on the map depends on it.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos_router::hooks::use_location;

use crate::SEQUOIA_WEBSITE_URL;
use crate::auth::{self, Viewer};
use crate::site_nav::{self, DISCORD_URL, MenuGroup, MenuSection, NavLeaf};

/// The resolved website session, or `None` while loading or when signed out.
///
/// Provided by `MapPage`. Display-only today; `Viewer::website_admin` gates the
/// Manage link, exactly as it does on the website.
#[derive(Clone, Copy)]
pub(crate) struct CurrentViewer(pub RwSignal<Option<Viewer>>);

/// Sentinel for the open-menu signal. Section keys are paths, so this cannot
/// collide with one.
const ACCOUNT_MENU: &str = "#account";

fn site_url(path: &str) -> String {
    format!("{SEQUOIA_WEBSITE_URL}{path}")
}

/// The sign-in href, recomputed on every router navigation.
///
/// `return_to` has to name the page the viewer is on *when they click*, not the
/// one the bar happened to render on - the map rewrites the path as the mode
/// changes (`/` <-> `/history`), so a value captured once sends them back to the
/// wrong view.
fn login_href() -> impl Fn() -> String + Copy {
    let location = use_location();
    let (pathname, search, hash) = (location.pathname, location.search, location.hash);
    move || {
        pathname.track();
        search.track();
        hash.track();
        auth::login_url(&auth::current_url())
    }
}

#[component]
pub fn SiteNavbar() -> impl IntoView {
    let CurrentViewer(viewer) = expect_context();

    // Which menu is open, keyed by section path (or ACCOUNT_MENU). Hover opens
    // on desktop; under the bar's breakpoint the panel below takes over.
    let open = RwSignal::new(None::<String>);
    let mobile_open = RwSignal::new(false);

    // The website's panel closes when the page scrolls out from under it. The
    // map does not scroll the window, so this is carried over for parity.
    let _ = window_event_listener(ev::scroll, move |_| {
        if mobile_open.get_untracked() {
            mobile_open.set(false);
        }
    });

    let sections = StoredValue::new(site_nav::visible_sections());

    view! {
        <nav class="seq-top-nav" on:mouseleave=move |_| open.set(None)>
            <div class="seq-top-nav-shell">
                <a
                    class="seq-nav-brand"
                    href=SEQUOIA_WEBSITE_URL
                    on:click=move |_| mobile_open.set(false)
                >
                    "SEQUOIA"
                </a>

                <div class="seq-nav-desktop">
                    {sections
                        .get_value()
                        .into_iter()
                        .map(|section| view! { <NavSectionItem section open /> })
                        .collect_view()}

                    // This *is* the map, so the site's Map link is a static
                    // marker here rather than a link back to ourselves.
                    <span class="seq-nav-link seq-nav-accent" aria-current="page">
                        "Map"
                    </span>
                    <a
                        class="seq-nav-link seq-nav-accent"
                        href=DISCORD_URL
                        target="_blank"
                        rel="noopener noreferrer"
                    >
                        "Discord"
                    </a>
                    <Show when=move || viewer.get().is_some_and(|current| current.website_admin)>
                        <a class="seq-nav-link" href=site_url("/manage")>
                            "Manage"
                        </a>
                    </Show>

                    <AccountControl viewer open />
                </div>

                <button
                    class="seq-nav-burger"
                    aria-label="Toggle menu"
                    aria-expanded=move || mobile_open.get().to_string()
                    on:click=move |_| mobile_open.update(|is_open| *is_open = !*is_open)
                >
                    <span class:seq-nav-burger-line-open-top=move || mobile_open.get()></span>
                    <span class:seq-nav-burger-line-hidden=move || mobile_open.get()></span>
                    <span class:seq-nav-burger-line-open-bottom=move || mobile_open.get()></span>
                </button>
            </div>

            <Show when=move || mobile_open.get()>
                <MobilePanel sections viewer mobile_open />
            </Show>
        </nav>
    }
}

#[component]
fn NavSectionItem(section: MenuSection, open: RwSignal<Option<String>>) -> impl IntoView {
    let path = section.path;
    let is_open = move || open.get().as_deref() == Some(path);
    let has_menu = section.has_menu();
    let section = StoredValue::new(section);

    view! {
        <span
            class="seq-nav-item"
            on:mouseenter=move |_| open.set(Some(path.to_string()))
        >
            <a class="seq-nav-link" href=site_url(path)>
                {section.with_value(|section| section.label)}
                <Show when=move || has_menu>
                    <Chevron open=Signal::derive(is_open) />
                </Show>
            </a>

            <Show when=move || is_open() && has_menu>
                <Dropdown>
                    {section
                        .get_value()
                        .groups
                        .into_iter()
                        .map(|group| view! { <DropdownGroup group /> })
                        .collect_view()}
                    <Show when=move || !section.with_value(|section| section.items.is_empty())>
                        <div class="seq-dropdown-col">
                            {section
                                .get_value()
                                .items
                                .into_iter()
                                .map(|leaf| view! { <DropdownItem leaf /> })
                                .collect_view()}
                        </div>
                    </Show>
                </Dropdown>
            </Show>
        </span>
    }
}

/// A menu panel that stays on screen: the website's `clampToViewport` action,
/// which slides a menu back by however far it would run past the right edge.
/// The account menu at the end of the bar is the one that needs it.
#[component]
fn Dropdown(children: ChildrenFn) -> impl IntoView {
    let panel = NodeRef::<html::Div>::new();

    Effect::new(move || {
        let Some(node) = panel.get() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let viewport = window
            .inner_width()
            .ok()
            .and_then(|width| width.as_f64())
            .unwrap_or_default();
        let overflow = node.get_bounding_client_rect().right() - (viewport - 16.0);
        if overflow > 0.0 {
            let style = web_sys::HtmlElement::style(&node);
            let _ = style.set_property("left", &format!("-{overflow}px"));
        }
    });

    view! {
        <div class="seq-dropdown" node_ref=panel>
            {children()}
        </div>
    }
}

#[component]
fn DropdownGroup(group: MenuGroup) -> impl IntoView {
    view! {
        <div class="seq-dropdown-col">
            <div class="seq-dropdown-group-header">
                <NavIcon path=group.icon size="14" />
                <a class="seq-dropdown-group-label" href=site_url(group.path)>
                    {group.label}
                </a>
            </div>
            {group
                .items
                .into_iter()
                .map(|leaf| view! { <DropdownItem leaf /> })
                .collect_view()}
        </div>
    }
}

#[component]
fn DropdownItem(leaf: NavLeaf) -> impl IntoView {
    view! {
        <a class="seq-dropdown-item" href=site_url(leaf.path)>
            <span class="seq-dropdown-item-icon">
                <NavIcon path=leaf.icon size="17" />
            </span>
            <span class="seq-dropdown-item-text">
                <span class="seq-dropdown-item-label">{leaf.label}</span>
                <span class="seq-dropdown-item-desc">{leaf.description}</span>
            </span>
        </a>
    }
}

/// Signed-out: a Sign in link. Signed-in: the player's face and name, with a
/// hover menu carrying Playercard / Manage / Settings / Sign out.
#[component]
fn AccountControl(
    viewer: RwSignal<Option<Viewer>>,
    open: RwSignal<Option<String>>,
) -> impl IntoView {
    let is_open = move || open.get().as_deref() == Some(ACCOUNT_MENU);
    let login_href = login_href();

    move || {
        let Some(current) = viewer.get() else {
            return view! {
                // `rel="external"` keeps the router's global anchor handler off
                // this click: `/api/auth/*` is same-origin, so without it the
                // router turns sign-in into a client-side route change and the
                // request never reaches the server.
                <a class="seq-nav-link seq-nav-session" href=login_href rel="external">
                    "Sign in"
                </a>
            }
            .into_any();
        };

        let face_small = auth::nmsr_face(&current.minecraft_uuid, 18);
        let playercard = playercard_url(&current);
        // Without a linked player there is no card to open, so the chip points
        // at account settings - never sign-out, which would turn clicking your
        // own name into a logout.
        let chip_href = playercard
            .clone()
            .unwrap_or_else(|| site_url("/account/settings"));
        let chip_name = chip_name(&current);
        let manage = current.website_admin;
        // The menu is rebuilt on every hover, so what it draws has to survive
        // being read more than once.
        let face_large = StoredValue::new(auth::nmsr_face(&current.minecraft_uuid, 36));
        let playercard = StoredValue::new(playercard);
        let minecraft_name = StoredValue::new(
            current
                .minecraft_username
                .clone()
                .unwrap_or_else(|| "No linked player".to_string()),
        );
        let discord_handle = StoredValue::new(
            current
                .discord_username
                .clone()
                .map(|handle| format!("@{handle}"))
                .unwrap_or_default(),
        );

        view! {
            <span
                class="seq-nav-item"
                on:mouseenter=move |_| open.set(Some(ACCOUNT_MENU.to_string()))
            >
                <a class="seq-nav-link seq-nav-viewer" href=chip_href>
                    <img class="seq-nav-face" src=face_small alt="" width="18" height="18" />
                    {chip_name.clone()}
                    <Chevron open=Signal::derive(is_open) />
                </a>

                <Show when=is_open>
                    <Dropdown>
                        <div class="seq-dropdown-col seq-account-col">
                            <div class="seq-account-identity">
                                <img
                                    class="seq-account-face"
                                    src=move || face_large.get_value()
                                    alt=""
                                    width="36"
                                    height="36"
                                />
                                <span class="seq-account-names">
                                    <span class="seq-account-minecraft">
                                        {move || minecraft_name.get_value()}
                                    </span>
                                    <span class="seq-account-discord">
                                        {move || discord_handle.get_value()}
                                    </span>
                                </span>
                            </div>
                            {move || {
                                playercard
                                    .get_value()
                                    .map(|href| view! { <AccountMenuItem href label="Playercard" /> })
                            }}
                            {manage
                                .then(|| {
                                    view! {
                                        <AccountMenuItem href=site_url("/manage") label="Manage" />
                                    }
                                })}
                            <AccountMenuItem href=site_url("/account/settings") label="Settings" />
                            <AccountMenuItem href=auth::logout_url() label="Sign out" />
                        </div>
                    </Dropdown>
                </Show>
            </span>
        }
        .into_any()
    }
}

/// The bar under its breakpoint: the same tree, stacked, behind the burger.
#[component]
fn MobilePanel(
    sections: StoredValue<Vec<MenuSection>>,
    viewer: RwSignal<Option<Viewer>>,
    mobile_open: RwSignal<bool>,
) -> impl IntoView {
    let close = move |_| mobile_open.set(false);
    let login_href = login_href();

    view! {
        <div class="seq-nav-mobile-panel">
            <div class="seq-nav-mobile-links">
                {sections
                    .get_value()
                    .into_iter()
                    .map(|section| {
                        view! {
                            <a class="seq-mobile-link" href=site_url(section.path) on:click=close>
                                {section.label}
                            </a>
                            {section
                                .groups
                                .into_iter()
                                .map(|group| {
                                    view! {
                                        <div class="seq-mobile-subgroup-label">{group.label}</div>
                                        {group
                                            .items
                                            .into_iter()
                                            .map(|leaf| view! { <MobileSublink leaf close /> })
                                            .collect_view()}
                                    }
                                })
                                .collect_view()}
                            {section
                                .items
                                .into_iter()
                                .map(|leaf| view! { <MobileSublink leaf close /> })
                                .collect_view()}
                        }
                    })
                    .collect_view()}

                <span class="seq-mobile-link seq-nav-accent" aria-current="page">
                    "Map"
                </span>
                <a
                    class="seq-mobile-link seq-nav-accent"
                    href=DISCORD_URL
                    target="_blank"
                    rel="noopener noreferrer"
                    on:click=close
                >
                    "Discord"
                </a>

                {move || {
                    let Some(current) = viewer.get() else {
                        return view! {
                            <a class="seq-mobile-link" href=login_href rel="external">
                                "Sign in"
                            </a>
                        }
                            .into_any();
                    };
                    let face = auth::nmsr_face(&current.minecraft_uuid, 18);
                    let chip_name = chip_name(&current);
                    view! {
                        {playercard_url(&current)
                            .map(|href| {
                                view! {
                                    <a
                                        class="seq-mobile-link seq-mobile-viewer"
                                        href=href
                                        on:click=close
                                    >
                                        <img
                                            class="seq-nav-face"
                                            src=face
                                            alt=""
                                            width="18"
                                            height="18"
                                        />
                                        {chip_name}
                                    </a>
                                }
                            })}
                        {current
                            .website_admin
                            .then(|| {
                                view! {
                                    <a
                                        class="seq-mobile-link"
                                        href=site_url("/manage")
                                        on:click=close
                                    >
                                        "Manage"
                                    </a>
                                }
                            })}
                        <a
                            class="seq-mobile-link"
                            href=site_url("/account/settings")
                            on:click=close
                        >
                            "Settings"
                        </a>
                        <a class="seq-mobile-link" href=auth::logout_url() rel="external">
                            "Sign out"
                        </a>
                    }
                        .into_any()
                }}
            </div>
        </div>
    }
}

#[component]
fn MobileSublink(leaf: NavLeaf, close: impl Fn(ev::MouseEvent) + 'static) -> impl IntoView {
    view! {
        <a class="seq-mobile-sublink" href=site_url(leaf.path) on:click=close>
            {leaf.label}
        </a>
    }
}

#[component]
fn AccountMenuItem(href: String, label: &'static str) -> impl IntoView {
    view! {
        // Every account destination leaves the map - the website for Playercard
        // / Manage / Settings, `/api/auth/logout` for Sign out. The latter is
        // same-origin, so without `rel="external"` the router would swallow the
        // click and sign-out would silently do nothing.
        <a class="seq-dropdown-item" href=href rel="external">
            <span class="seq-dropdown-item-text">
                <span class="seq-dropdown-item-label">{label}</span>
            </span>
        </a>
    }
}

#[component]
fn Chevron(open: Signal<bool>) -> impl IntoView {
    view! {
        <svg
            class="seq-nav-chevron"
            class:seq-nav-chevron-open=move || open.get()
            width="11"
            height="11"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            aria-hidden="true"
        >
            <path stroke-linecap="round" stroke-linejoin="round" d="M6 9l6 6 6-6" />
        </svg>
    }
}

#[component]
fn NavIcon(path: &'static str, size: &'static str) -> impl IntoView {
    view! {
        <svg
            width=size
            height=size
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <path d=path />
        </svg>
    }
}

/// The name on the chip: the linked player, or a neutral label. Matches the
/// website's `viewer.minecraftUsername ?? "Account"`.
fn chip_name(viewer: &Viewer) -> String {
    viewer
        .minecraft_username
        .clone()
        .unwrap_or_else(|| "Account".to_string())
}

/// The viewer's own playercard, when there is a linked player to open one for.
fn playercard_url(viewer: &Viewer) -> Option<String> {
    viewer
        .minecraft_username
        .as_deref()
        .map(crate::player_card_url)
}
