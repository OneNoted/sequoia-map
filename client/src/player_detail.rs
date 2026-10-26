//! A member's detail, opened from any name in the war HUD.
//!
//! Everything here comes from the war controller feed and the territory map; it renders only
//! inside [`crate::war_room::WarRoomHud`], so it shares the members-only, live-only gate. A
//! member who drops out of the feed while open keeps their last reported details, labelled
//! as no longer reporting, rather than vanishing under the reader.

use leptos::prelude::*;
use sequoia_shared::war_board::{FrontStage, Presence, RosterMember, WarFront};
use wasm_bindgen::JsCast;

use crate::territory::ClientTerritoryMap;
use crate::war_room::{ClassIcon, MapFocus, MemberChip, WarRoom, clock_text, whereabouts};

const FACE_PX: u32 = 64;
const FOCUSABLE: &str = "button:not([disabled]), a[href], [tabindex]:not([tabindex='-1'])";

/// The member's latest entry, or their last one with `reporting: false` once they leave
/// the feed.
#[derive(Clone, PartialEq)]
struct Shown {
    member: RosterMember,
    reporting: bool,
}

#[component]
pub(crate) fn PlayerDetail() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let focus_map = MapFocus::from_context();
    let dialog_ref = NodeRef::<leptos::html::Div>::new();
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);

    let shown = Memo::new(move |previous: Option<&Option<Shown>>| {
        let username = room.player.get()?;
        let live = room
            .board
            .with(|board| board.as_ref()?.member(&username).cloned());
        match live {
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
    // A name that was never in the feed has nothing to show.
    Effect::new(move || {
        if room.player.with(Option::is_some) && shown.with(Option::is_none) {
            room.player.set(None);
        }
    });

    let close = move || {
        room.player.set(None);
        if let Some(opener) = opener.get_value() {
            let _ = opener.focus();
        }
    };
    // Remember what opened the dialog, then move focus into it.
    Effect::new(move |was_open: Option<bool>| {
        let open = shown.with(Option::is_some);
        if open && was_open != Some(true) {
            let active = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.active_element())
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
            opener.set_value(active);
            if let Some(dialog) = dialog_ref.get() {
                let _ = dialog.focus();
            }
        }
        open
    });

    let on_keydown = move |event: web_sys::KeyboardEvent| match event.key().as_str() {
        "Escape" => {
            event.prevent_default();
            close();
        }
        "Tab" => trap_focus(&event, dialog_ref.get()),
        _ => {}
    };

    view! {
        <Show when=move || shown.with(Option::is_some)>
            <div class="player-dialog-scrim" on:click=move |_| close() />
        </Show>
        <div
            class="player-dialog"
            class:player-dialog-open=move || shown.with(Option::is_some)
            role="dialog"
            aria-modal="true"
            aria-labelledby="player-dialog-name"
            tabindex="-1"
            node_ref=dialog_ref
            inert=move || shown.with(Option::is_none)
            on:keydown=on_keydown
        >
            {move || shown.get().map(|shown| {
                let username = shown.member.username.clone();
                let territory = shown.member.territory().map(str::to_string);
                let front = territory.as_deref().and_then(|territory| room.front(territory));
                let owner = territory.as_deref().and_then(|territory| {
                    territories.with(|map| {
                        let guild = &map.get(territory)?.territory.guild;
                        (!guild.is_unclaimed()).then(|| format!("[{}] {}", guild.prefix, guild.name))
                    })
                });
                let companions: Vec<RosterMember> = territory
                    .as_deref()
                    .map(|territory| {
                        room.board.with(|board| {
                            board.as_ref().map_or_else(Vec::new, |board| {
                                board
                                    .members_in(territory)
                                    .filter(|member| member.username != username)
                                    .cloned()
                                    .collect()
                            })
                        })
                    })
                    .unwrap_or_default();
                let target = match (&shown.member.presence, territory.clone()) {
                    (_, Some(territory)) => Some(Target::Territory(territory)),
                    (Presence::Online { position: Some(position), .. }, None) => {
                        Some(Target::Position(*position))
                    }
                    _ => None,
                };
                view! {
                    <PlayerDialogContent
                        shown
                        front
                        owner
                        companions
                        target
                        on_close=Callback::new(move |()| close())
                        focus_map
                    />
                }
            })}
        </div>
    }
}

#[derive(Clone)]
enum Target {
    Territory(String),
    Position((f64, f64)),
}

#[component]
fn PlayerDialogContent(
    shown: Shown,
    front: Option<WarFront>,
    owner: Option<String>,
    companions: Vec<RosterMember>,
    target: Option<Target>,
    on_close: Callback<()>,
    focus_map: MapFocus,
) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let member = shown.member;
    let (status, state) = match (shown.reporting, member.is_warring()) {
        (false, _) => ("No longer reporting".to_string(), "gone"),
        (true, true) => (
            format!("Warring at {}", member.territory().unwrap_or_default()),
            "warring",
        ),
        (true, false) => ("Online".to_string(), "online"),
    };
    let class_label = member
        .class
        .map_or("Unknown", sequoia_shared::PlayerClass::label);
    let place = whereabouts(&member);
    let position = member.position();
    let war = front.clone().map(|front| {
        let stage = front.stage_kind().label();
        let tower = match front.stage {
            FrontStage::Fighting {
                tower: Some(tower), ..
            } if tower.is_measured() => Some(format!(
                "tower {:.0}%",
                tower.health.clamp(0.0, 1.0) * 100.0
            )),
            _ => None,
        };
        move || {
            let (_, spoken) = clock_text(front.clock(room.now.get()));
            let mut line = format!("{stage}, {spoken}");
            if let Some(tower) = &tower {
                line.push_str(&format!(", {tower}"));
            }
            line
        }
    });
    let face = crate::auth::nmsr_face(&member.username, FACE_PX);
    let card = crate::player_card_url(&member.username);
    let can_show = target.is_some();
    let show = move |_| {
        match &target {
            Some(Target::Territory(territory)) => focus_map.territory(territory),
            Some(Target::Position(position)) => focus_map.position(*position),
            None => {}
        }
        on_close.run(());
    };

    view! {
        <div class="player-dialog-header">
            <img class="player-dialog-face" src=face alt="" width="48" height="48" />
            <div class="player-dialog-heading">
                <h2 class="player-dialog-name" id="player-dialog-name">{member.username.clone()}</h2>
                <p class="player-dialog-status" data-state=state>{status}</p>
            </div>
            <button type="button" class="war-icon-button" aria-label="Close" on:click=move |_| on_close.run(())>
                "\u{00D7}"
            </button>
        </div>
        <dl class="player-dialog-facts">
            <div>
                <dt>"Class"</dt>
                <dd class="player-dialog-class"><ClassIcon class=member.class />{class_label}</dd>
            </div>
            <div>
                <dt>"Standing in"</dt>
                <dd>
                    {place}
                    {owner.map(|owner| view! { <span class="player-dialog-owner">{owner}</span> })}
                </dd>
            </div>
            {position.map(|(x, z)| view! {
                <div>
                    <dt>"Position"</dt>
                    <dd class="player-dialog-mono">{format!("{x:.0}, {z:.0}")}</dd>
                </div>
            })}
            {war.map(|war| view! {
                <div>
                    <dt>"War"</dt>
                    <dd>{war}</dd>
                </div>
            })}
        </dl>
        {(!companions.is_empty()).then(|| view! {
            <section class="player-dialog-section" aria-labelledby="player-dialog-with">
                <h3 class="war-group-title" id="player-dialog-with">"Also here"</h3>
                <ul class="war-front-members">
                    {companions.into_iter().map(|member| view! { <li><MemberChip member /></li> }).collect_view()}
                </ul>
            </section>
        })}
        <div class="player-dialog-actions">
            <button type="button" class="war-action" disabled=!can_show on:click=show>"Show on map"</button>
            <a class="war-action war-action-quiet" href=card target="_blank" rel="noopener noreferrer">"Player card"</a>
        </div>
    }
}

/// Keeps Tab and Shift+Tab inside the dialog.
fn trap_focus(event: &web_sys::KeyboardEvent, dialog: Option<web_sys::HtmlDivElement>) {
    let Some(dialog) = dialog else {
        return;
    };
    let Ok(nodes) = dialog.query_selector_all(FOCUSABLE) else {
        return;
    };
    let focusable: Vec<web_sys::HtmlElement> = (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into().ok())
        .collect();
    let (Some(first), Some(last)) = (focusable.first(), focusable.last()) else {
        event.prevent_default();
        return;
    };
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
    let at = |element: &web_sys::HtmlElement| {
        active
            .as_ref()
            .is_some_and(|active| active.is_same_node(Some(element)))
    };
    let leaving_start = event.shift_key()
        && (at(first)
            || active
                .as_ref()
                .is_some_and(|a| a.is_same_node(Some(&dialog))));
    if leaving_start {
        event.prevent_default();
        let _ = last.focus();
    } else if !event.shift_key() && at(last) {
        event.prevent_default();
        let _ = first.focus();
    }
}
