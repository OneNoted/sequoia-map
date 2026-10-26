//! A member's detail, opened from any name in the war HUD.
//!
//! Everything here comes from the war controller feed and the territory map, and it renders
//! only inside [`super::WarRoomHud`], so it shares the members-only, live-only gate. A member
//! who drops out of the feed while open keeps their last reported details, labelled as no
//! longer reporting, rather than vanishing under the reader.
//!
//! The dialog is built once per member and its fields update in place, so a feed frame never
//! takes focus away from the button a keyboard user is on.
//!
//! On a desktop it is a popover beside the name that opened it, so the pointer has no trip to
//! make and the map stays usable around it; on a phone it is a sheet from the bottom.

use leptos::prelude::*;
use sequoia_shared::PlayerClass;
use sequoia_shared::war_board::{FrontStage, Presence, RosterMember};

use super::hud::{ClassIcon, MemberChip, member_key};
use super::modal::{focus_next_frame, focused_element, restore_focus, trap_tab};
use super::{Anchor, MapFocus, WarRoom, clock_text, whereabouts};
use crate::app::IsMobile;
use crate::territory::ClientTerritoryMap;

const FACE_PX: u32 = 64;
/// Space between the popover and the name it belongs to, and kept clear of the window edge.
const POPOVER_GAP: f64 = 8.0;
const POPOVER_MARGIN: f64 = 8.0;

/// Where a popover of `size` opens: right of `anchor`, or left of it when the right has no
/// room, top-aligned with it, and moved back inside the viewport if it would spill.
pub(super) fn popover_origin(anchor: Anchor, size: (f64, f64), viewport: (f64, f64)) -> (f64, f64) {
    let (width, height) = size;
    let (view_width, view_height) = viewport;
    let right = anchor.right + POPOVER_GAP;
    let x = if right + width + POPOVER_MARGIN <= view_width {
        right
    } else {
        anchor.left - POPOVER_GAP - width
    };
    let clamp = |value: f64, extent: f64, view: f64| {
        value
            .min(view - extent - POPOVER_MARGIN)
            .max(POPOVER_MARGIN)
    };
    (
        clamp(x, width, view_width),
        clamp(anchor.top - 4.0, height, view_height),
    )
}
/// Members listed under "Also here" before the rest are counted instead.
const COMPANIONS_SHOWN: usize = 8;

/// The member's latest entry, or their last one with `reporting: false` once they leave
/// the feed.
#[derive(Clone, PartialEq)]
struct Shown {
    member: RosterMember,
    reporting: bool,
}

#[component]
pub(super) fn PlayerDetail() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let IsMobile(is_mobile) = expect_context();
    let dialog_ref = NodeRef::<leptos::html::Div>::new();
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let popover = Memo::new(move |_| !is_mobile.get());
    let origin = RwSignal::new(None::<(f64, f64)>);

    let shown = Memo::new(move |previous: Option<&Option<Shown>>| {
        let username = room.player.get()?;
        match room.member(&username) {
            Some(member) => Some(Shown {
                member,
                reporting: true,
            }),
            None => previous
                .cloned()
                .flatten()
                .filter(|shown| shown.member.username == username)
                .map(|shown| Shown {
                    reporting: false,
                    ..shown
                }),
        }
    });
    let username = Memo::new(move |_| {
        shown.with(|shown| shown.as_ref().map(|shown| shown.member.username.clone()))
    });
    // A name that was never in the feed has nothing to show.
    Effect::new(move || {
        if room.player.with(Option::is_some) && username.with(Option::is_none) {
            room.player.set(None);
        }
    });
    // Focus moves in when the dialog opens and again when it switches to another member,
    // whose content replaces the button that was focused; it goes back to the opener when
    // the dialog closes.
    Effect::new(move |previous: Option<Option<String>>| {
        let current = username.get();
        let was_open = previous.as_ref().is_some_and(Option::is_some);
        match (&current, was_open) {
            (Some(_), false) => {
                opener.set_value(focused_element());
                if let Some(dialog) = dialog_ref.get() {
                    focus_next_frame(dialog.into());
                }
            }
            (Some(name), true) if previous.flatten().as_ref() != Some(name) => {
                if let Some(dialog) = dialog_ref.get() {
                    focus_next_frame(dialog.into());
                }
            }
            (None, true) => restore_focus(opener.get_value()),
            _ => {}
        }
        current
    });

    // Placed beside its name once it has a size, and again whenever its content changes.
    Effect::new(move || {
        shown.track();
        let anchor = room.player_anchor.get();
        if !popover.get() || username.with(Option::is_none) {
            origin.set(None);
            return;
        }
        let Some(anchor) = anchor else {
            origin.set(None);
            return;
        };
        if origin.get_untracked().is_none() {
            // A first guess for the frame before it can be measured.
            origin.set(Some((anchor.right + POPOVER_GAP, anchor.top)));
        }
        request_animation_frame(move || {
            let (Some(dialog), Some(window)) = (dialog_ref.get_untracked(), web_sys::window())
            else {
                return;
            };
            let viewport = (
                window
                    .inner_width()
                    .ok()
                    .and_then(|w| w.as_f64())
                    .unwrap_or(0.0),
                window
                    .inner_height()
                    .ok()
                    .and_then(|h| h.as_f64())
                    .unwrap_or(0.0),
            );
            let size = (
                f64::from(dialog.offset_width()),
                f64::from(dialog.offset_height()),
            );
            origin.set(Some(popover_origin(anchor, size, viewport)));
        });
    });

    let close = Callback::new(move |()| room.player.set(None));
    let on_keydown = move |event: web_sys::KeyboardEvent| match event.key().as_str() {
        "Escape" => {
            event.prevent_default();
            close.run(());
        }
        "Tab" => {
            if let Some(dialog) = dialog_ref.get() {
                trap_tab(&event, &dialog);
            }
        }
        _ => {}
    };

    view! {
        <Show when=move || username.with(Option::is_some) && !popover.get()>
            <div class="player-dialog-scrim" on:click=move |_| close.run(()) />
        </Show>
        <div
            class="player-dialog"
            class:player-dialog-open=move || username.with(Option::is_some)
            class:player-dialog-popover=move || popover.get()
            class:player-dialog-placed=move || origin.with(Option::is_some)
            style:left=move || origin.get().map(|(x, _)| format!("{x:.0}px"))
            style:top=move || origin.get().map(|(_, y)| format!("{y:.0}px"))
            role="dialog"
            aria-modal=move || (!popover.get()).to_string()
            aria-labelledby="player-dialog-name"
            tabindex="-1"
            node_ref=dialog_ref
            inert=move || username.with(Option::is_none)
            on:keydown=on_keydown
        >
            {move || {
                username
                    .get()
                    .map(|name| view! { <PlayerDialogContent username=name shown on_close=close /> })
            }}
        </div>
    }
}

#[component]
fn PlayerDialogContent(
    username: String,
    shown: Memo<Option<Shown>>,
    on_close: Callback<()>,
) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let focus_map = MapFocus::from_context();

    let member = move || shown.with(|shown| shown.as_ref().map(|shown| shown.member.clone()));
    let status = Memo::new(move |_| {
        shown.with(|shown| match shown.as_ref() {
            Some(shown) if !shown.reporting => ("No longer reporting".to_string(), "gone"),
            Some(shown) => match &shown.member.presence {
                Presence::Warring { territory } => (format!("Warring at {territory}"), "warring"),
                Presence::Online { .. } => ("Online".to_string(), "online"),
            },
            None => (String::new(), "gone"),
        })
    });
    let class = Memo::new(move |_| member().and_then(|member| member.class));
    let territory =
        Memo::new(move |_| member().and_then(|member| member.territory().map(str::to_string)));
    let place = Memo::new(move |_| member().as_ref().map(whereabouts).unwrap_or_default());
    let position = Memo::new(move |_| member().and_then(|member| member.position()));
    let owner = Memo::new(move |_| {
        let territory = territory.get()?;
        territories.with(|map| {
            let guild = &map.get(&territory)?.territory.guild;
            (!guild.is_unclaimed()).then(|| format!("[{}] {}", guild.prefix, guild.name))
        })
    });
    let front = Memo::new(move |_| territory.get().and_then(|territory| room.front(&territory)));
    let war = move || {
        front.with(|front| {
            let front = front.as_ref()?;
            let (_, spoken) = clock_text(front.clock(room.now.get()));
            let mut line = format!("{}, {spoken}", front.stage_kind().label());
            if let FrontStage::Fighting {
                tower: Some(tower), ..
            } = front.stage
                && tower.is_measured()
            {
                line.push_str(&format!(
                    ", tower {:.0}%",
                    tower.health.clamp(0.0, 1.0) * 100.0
                ));
            }
            Some(line)
        })
    };
    let companions = {
        let username = username.clone();
        Memo::new(move |_| {
            territory
                .get()
                .map(|territory| {
                    room.members_in(&territory)
                        .into_iter()
                        .filter(|member| member.username != username)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    let shown_companions = Memo::new(move |_| {
        companions.with(|list| {
            list.iter()
                .take(COMPANIONS_SHOWN)
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let more_companions =
        Memo::new(move |_| companions.with(|list| list.len().saturating_sub(COMPANIONS_SHOWN)));
    let has_more = Memo::new(move |_| more_companions.get() != 0);
    let can_show = Memo::new(move |_| territory.get().is_some() || position.get().is_some());
    let show = move |_| {
        if let Some(territory) = territory.get_untracked() {
            focus_map.territory(&territory);
        } else if let Some(position) = position.get_untracked() {
            focus_map.position(position);
        }
        on_close.run(());
    };
    let face = crate::auth::nmsr_face(&username, FACE_PX);
    let card = crate::player_card_url(&username);

    view! {
        <div class="player-dialog-header">
            <img class="player-dialog-face" src=face alt="" width="48" height="48" />
            <div class="player-dialog-heading">
                <h2 class="player-dialog-name" id="player-dialog-name">{username.clone()}</h2>
                <p class="player-dialog-status" data-state=move || status.get().1>{move || status.get().0}</p>
            </div>
            <button type="button" class="war-icon-button" aria-label="Close" on:click=move |_| on_close.run(())>
                "\u{00D7}"
            </button>
        </div>
        <dl class="player-dialog-facts">
            <div>
                <dt>"Class"</dt>
                <dd class="player-dialog-class">
                    {move || view! { <ClassIcon class=class.get() /> }}
                    {move || class.get().map_or("Unknown", PlayerClass::label)}
                </dd>
            </div>
            <div>
                <dt>"Standing in"</dt>
                <dd>
                    {move || place.get()}
                    {move || owner.get().map(|owner| view! { <span class="player-dialog-owner">{owner}</span> })}
                </dd>
            </div>
            <Show when=move || position.get().is_some()>
                <div>
                    <dt>"Position"</dt>
                    <dd class="player-dialog-mono">
                        {move || position.get().map(|(x, z)| format!("{x:.0}, {z:.0}")).unwrap_or_default()}
                    </dd>
                </div>
            </Show>
            <Show when=move || front.with(Option::is_some)>
                <div>
                    <dt>"War"</dt>
                    <dd>{move || war().unwrap_or_default()}</dd>
                </div>
            </Show>
        </dl>
        <Show when=move || !companions.with(Vec::is_empty)>
            <section class="player-dialog-section" aria-labelledby="player-dialog-with">
                <h3 class="war-group-title" id="player-dialog-with">"Also here"</h3>
                <ul class="war-front-members">
                    <For each=move || shown_companions.get() key=member_key let:member>
                        <li><MemberChip member /></li>
                    </For>
                    <Show when=move || has_more.get()>
                        <li class="player-dialog-more">{move || format!("and {} more", more_companions.get())}</li>
                    </Show>
                </ul>
            </section>
        </Show>
        <div class="player-dialog-actions">
            <button type="button" class="war-action" disabled=move || !can_show.get() on:click=show>
                "Show on map"
            </button>
            <a class="war-action war-action-quiet" href=card target="_blank" rel="noopener noreferrer">"Player card"</a>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f64, f64) = (1440.0, 900.0);
    const SIZE: (f64, f64) = (300.0, 260.0);

    fn anchor(left: f64, top: f64, width: f64) -> Anchor {
        Anchor {
            left,
            top,
            right: left + width,
            bottom: top + 26.0,
        }
    }

    #[test]
    fn a_popover_opens_right_of_a_roster_name() {
        assert_eq!(
            popover_origin(anchor(30.0, 300.0, 120.0), SIZE, VIEW),
            (158.0, 296.0)
        );
    }

    #[test]
    fn a_name_near_the_right_edge_gets_its_popover_on_the_left() {
        assert_eq!(
            popover_origin(anchor(1250.0, 300.0, 120.0), SIZE, VIEW),
            (942.0, 296.0)
        );
    }

    #[test]
    fn a_name_near_the_bottom_lifts_its_popover_inside_the_window() {
        let (_, y) = popover_origin(anchor(30.0, 860.0, 120.0), SIZE, VIEW);
        assert_eq!(y, VIEW.1 - SIZE.1 - POPOVER_MARGIN);
    }

    #[test]
    fn a_window_narrower_than_the_popover_keeps_its_left_edge_visible() {
        assert_eq!(
            popover_origin(anchor(10.0, 10.0, 40.0), SIZE, (200.0, 900.0)).0,
            POPOVER_MARGIN
        );
    }
}
