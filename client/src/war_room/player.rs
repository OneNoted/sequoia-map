//! A member's detail, opened from any name in the war HUD.
//!
//! Everything here comes from the war controller feed and the territory map, and it renders
//! only inside [`super::WarRoomHud`], so it shares the members-only, live-only gate. A member
//! who drops out of the feed while open keeps their last reported details, labelled as no
//! longer reporting, rather than vanishing under the reader.
//!
//! The dialog is built once per member and its fields update in place, so a feed frame never
//! takes focus away from the button a keyboard user is on.

use leptos::prelude::*;
use sequoia_shared::PlayerClass;
use sequoia_shared::war_board::{FrontStage, Presence, RosterMember};

use super::hud::{ClassIcon, MemberChip, member_key};
use super::modal::{focus_next_frame, focused_element, restore_focus, trap_tab};
use super::{MapFocus, WarRoom, clock_text, whereabouts};
use crate::territory::ClientTerritoryMap;

const FACE_PX: u32 = 64;
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
    let dialog_ref = NodeRef::<leptos::html::Div>::new();
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);

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
        <Show when=move || username.with(Option::is_some)>
            <div class="player-dialog-scrim" on:click=move |_| close.run(()) />
        </Show>
        <div
            class="player-dialog"
            class:player-dialog-open=move || username.with(Option::is_some)
            role="dialog"
            aria-modal="true"
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
                        <li class="war-front-empty">{move || format!("and {} more", more_companions.get())}</li>
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
