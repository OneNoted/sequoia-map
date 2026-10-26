//! The war HUD: the side panel, the phone summary and sheet, and the rows inside them.

use std::rc::Rc;

use leptos::prelude::*;
use sequoia_browser_map::BrowserMap;
use sequoia_shared::PlayerClass;
use sequoia_shared::tower::format_stat;
use sequoia_shared::war_board::{
    Ending, FrontStage, QUEUE_ENTRY_TO_START_SECS, RosterMember, StageKind, Tower, WarFront,
    format_clock,
};
use wasm_bindgen::JsCast;

use super::modal::{focus_next_frame, focused_element, restore_focus, trap_tab};
use super::plates::WarPlatesLayer;
use super::player::PlayerDetail;
use super::{
    MapFocus, WarRoom, clock_text, difficulty_color, difficulty_label, ending, stage_slug,
    whereabouts,
};
use crate::app::{
    IsMobile, PeekTerritory, ShowWarPanel, SidebarOpen, WarPanelOpen, WarPanelWidth,
    clamp_war_panel_width,
};
use crate::territory::ClientTerritoryMap;
use crate::territory_card::{QueueOrigin, local_time, local_time_seconds, original_timer};
use crate::ui_icons::class_icon_url;

/// At or below this viewport height (CSS pixels) the side panel would crowd the map, so the
/// HUD uses the phone's summary button and drawer, as `input.css` does for the drawer.
const COMPACT_HUD_MAX_HEIGHT: f64 = 520.0;
const STAGES: [StageKind; 3] = [StageKind::Fighting, StageKind::Entered, StageKind::Queued];
/// Member chips a collapsed territory row shows before summing up the rest.
const MEMBER_PREVIEW: usize = 6;

/// The war HUD: a side panel on roomy screens; a summary button and a sheet (a drawer on
/// short landscape screens) on phones.
#[component]
pub(crate) fn WarRoomHud() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let ShowWarPanel(show_panel) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let camera = expect_context::<BrowserMap>().camera();
    let visible = Memo::new(move |_| show_panel.get() && room.board.with(Option::is_some));
    let compact = Memo::new(move |_| {
        is_mobile.get() || camera.canvas_size_tracked().1 <= COMPACT_HUD_MAX_HEIGHT
    });

    view! {
        <Show when=move || visible.get()>
            <Show when=move || !compact.get() fallback=|| view! { <WarSummaryButton /> <WarSheet /> }>
                <WarPanel />
            </Show>
        </Show>
        <WarPlatesLayer />
        <PlayerDetail />
    }
}

#[component]
fn WarPanel() -> impl IntoView {
    let WarPanelOpen(panel_open) = expect_context();
    let WarPanelWidth(panel_width) = expect_context();
    // Transitions only after mount, so a panel restored as collapsed does not animate shut.
    let ready = RwSignal::new(false);
    Effect::new(move |_| ready.set(true));

    view! {
        <aside
            class="war-panel"
            aria-label="War"
            class:war-panel-ready=move || ready.get()
            class:war-panel-open=move || panel_open.get()
            style:width=move || format!("{:.0}px", panel_width.get())
        >
            <WarPanelToggle panel_open />
            <WarPanelResizeHandle />
            <div class="war-panel-card" inert=move || !panel_open.get()>
                <WarRoomHeader />
                <div class="war-panel-body scrollbar-thin">
                    <WarRoomBody />
                </div>
            </div>
        </aside>
    }
}

#[component]
fn WarRoomHeader(#[prop(optional)] on_close: Option<Callback<()>>) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    view! {
        <div class="war-room-header">
            <h2 class="war-room-title">"War"</h2>
            <Show when=move || room.stale.get()>
                <span class="war-room-stale" role="status">"Reconnecting"</span>
            </Show>
            {on_close.map(|close| view! {
                <button type="button" class="war-icon-button" aria-label="Close war panel" on:click=move |_| close.run(())>
                    "\u{00D7}"
                </button>
            })}
        </div>
    }
}

/// Fronts, recent outcomes and the member roster: the panel's and the sheet's content.
#[component]
fn WarRoomBody() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let unavailable = Memo::new(move |_| {
        room.board
            .with(|board| board.as_ref().is_some_and(|board| board.unavailable))
    });
    let stages = Memo::new(move |_| {
        room.board.with(|board| {
            board.as_ref().map_or_else(Vec::new, |board| {
                STAGES
                    .into_iter()
                    .filter(|kind| board.count(*kind) > 0)
                    .collect::<Vec<_>>()
            })
        })
    });
    let recent = Memo::new(move |_| {
        room.outcomes
            .with(|list| list.iter().rev().cloned().collect::<Vec<_>>())
    });

    // An empty section is left out rather than explained; only a missing feed says so.
    view! {
        <Show
            when=move || !unavailable.get()
            fallback=|| view! { <p class="war-empty" role="status">"War feed unavailable"</p> }
        >
            <Show when=move || !recent.with(Vec::is_empty) || !stages.with(Vec::is_empty)>
                <section class="war-section war-section-fronts scrollbar-thin" aria-label="Territories">
                    <Show when=move || !recent.with(Vec::is_empty)>
                        <ul class="war-outcomes">
                            <For each=move || recent.get() key=|outcome| (outcome.territory.clone(), outcome.at) let:outcome>
                                <OutcomeItem territory=outcome.territory outcome_at=outcome.at />
                            </For>
                        </ul>
                    </Show>
                    <For each=move || stages.get() key=|kind| *kind let:kind>
                        <StageGroup kind />
                    </For>
                </section>
            </Show>
            <RosterSection />
        </Show>
    }
}

/// One stage's territories. Keyed by stage, and its rows by territory, so a territory joining
/// or leaving leaves every other row, and whatever has focus in it, in place.
#[component]
fn StageGroup(kind: StageKind) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let territories = Memo::new(move |_| {
        room.board.with(|board| {
            board.as_ref().map_or_else(Vec::new, |board| {
                board
                    .fronts
                    .iter()
                    .filter(|front| front.stage_kind() == kind)
                    .map(|front| front.territory.clone())
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        <div class="war-group" data-stage=stage_slug(kind)>
            <h4 class="war-group-title">
                {kind.label()}
                <span class="war-count">{move || territories.with(Vec::len)}</span>
            </h4>
            <ol class="war-fronts">
                <For each=move || territories.get() key=|territory| territory.clone() let:territory>
                    <FrontItem territory />
                </For>
            </ol>
        </div>
    }
}

#[component]
fn OutcomeItem(territory: String, outcome_at: i64) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let focus_map = MapFocus::from_context();
    let shown_ending = {
        let territory = territory.clone();
        Memo::new(move |_| {
            room.outcomes.with(|list| {
                let outcome = list
                    .iter()
                    .find(|outcome| outcome.territory == territory && outcome.at == outcome_at)?;
                Some(territories.with(|map| ending(outcome, map)))
            })
        })
    };
    let (slug, text) = (
        move || match shown_ending.get() {
            Some(Ending::Captured) => "captured",
            Some(Ending::Cancelled) => "cancelled",
            Some(Ending::Ended) | None => "ended",
        },
        move || match shown_ending.get() {
            Some(Ending::Captured) => "Captured",
            Some(Ending::Cancelled) => "Queue cancelled",
            Some(Ending::Ended) | None => "War ended",
        },
    );
    let focus = territory.clone();
    view! {
        <li class="war-outcome" data-outcome=slug>
            <button type="button" class="war-link" on:click=move |_| focus_map.territory(&focus)>
                {territory}
            </button>
            <span class="war-outcome-label">{text}</span>
        </li>
    }
}

/// One territory's row, reading its front from the board so it stays current.
#[component]
fn FrontItem(territory: String) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let focus_map = MapFocus::from_context();
    let front = {
        let territory = territory.clone();
        Memo::new(move |_| room.front(&territory))
    };
    let clock = Memo::new(move |_| {
        front.with(|front| {
            front
                .as_ref()
                .map(|front| clock_text(front.clock(room.now.get())))
        })
    });
    let members = {
        let territory = territory.clone();
        Memo::new(move |_| room.members_in(&territory))
    };
    let difficulty = Memo::new(move |_| {
        front.with(|front| front.as_ref().and_then(|front| front.difficulty.clone()))
    });
    let tower = Memo::new(move |_| {
        front.with(|front| match front.as_ref().map(|front| front.stage) {
            Some(FrontStage::Fighting {
                tower,
                expected_win_at,
                ..
            }) => Some((tower, expected_win_at)),
            _ => None,
        })
    });
    let expanded = RwSignal::new(false);
    let shown_members = Memo::new(move |_| {
        members.with(|members| {
            let limit = if expanded.get() {
                members.len()
            } else {
                MEMBER_PREVIEW
            };
            members.iter().take(limit).cloned().collect::<Vec<_>>()
        })
    });
    let hidden_members = move || members.with(Vec::len) - shown_members.with(Vec::len);
    let focus = territory.clone();
    let detail_label = format!("Queue details for {territory}");
    let name_title = {
        let territory = territory.clone();
        move || match difficulty_label(difficulty.get().as_deref()) {
            Some(level) => format!("Show {territory} on the map ({level} difficulty)"),
            None => format!("Show {territory} on the map"),
        }
    };

    view! {
        <li class="war-front">
            <div class="war-front-head">
                <button type="button" class="war-front-name" title=name_title on:click=move |_| focus_map.territory(&focus)>
                    <span class="war-difficulty" style:background=move || difficulty_color(difficulty.get().as_deref()) aria-hidden="true" />
                    <span class="war-front-territory">{territory.clone()}</span>
                </button>
                <span
                    class="war-clock"
                    role="timer"
                    aria-label=move || clock.get().map(|(_, spoken)| spoken).unwrap_or_default()
                >
                    {move || clock.get().map(|(visible, _)| visible).unwrap_or_default()}
                </span>
                <Show when=move || front.with(|front| front.as_ref().is_some_and(|front| !front.is_fighting()))>
                    <button
                        type="button"
                        class="war-front-expand"
                        aria-label=detail_label.clone()
                        title="Queue details"
                        aria-expanded=move || expanded.get().to_string()
                        on:click=move |_| expanded.update(|open| *open = !*open)
                    />
                </Show>
            </div>
            {move || tower.get().map(|(tower, win_at)| view! { <TowerLine tower win_at /> })}
            // A fighting front's row expands only for its members: its queue and start no longer
            // apply.
            <Show when=move || expanded.get() && front.with(|front| front.as_ref().is_some_and(|front| !front.is_fighting()))>
                {move || front.get().map(|front| view! { <FrontDetail front /> })}
            </Show>
            <Show when=move || !members.with(Vec::is_empty)>
                <ul class="war-front-members" aria-label="Members inside">
                    <For each=move || shown_members.get() key=member_key let:member>
                        <li><MemberChip member /></li>
                    </For>
                    <Show when=move || { hidden_members() > 0 }>
                        <li>
                            <button
                                type="button"
                                class="war-member war-member-more"
                                aria-label=move || format!("Show {} more members", hidden_members())
                                on:click=move |_| expanded.set(true)
                            >
                                {move || format!("+{}", hidden_members())}
                            </button>
                        </li>
                    </Show>
                </ul>
            </Show>
        </li>
    }
}

/// The expanded part of a territory row: who queued it and when, and the queue's timing.
#[component]
fn FrontDetail(front: WarFront) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let origin = front
        .queued
        .as_ref()
        .and_then(|queued| QueueOrigin::new(queued, room.now.get_untracked(), local_time));
    let queued = matches!(front.stage, FrontStage::Queued { .. });
    let starts_at = match front.stage {
        FrontStage::Queued { starts_at, .. } | FrontStage::Entered { starts_at } => starts_at,
        FrontStage::Fighting { .. } => None,
    };
    let time = |at: Option<i64>| at.map_or_else(|| "Unknown".to_string(), local_time_seconds);
    view! {
        <dl class="war-front-detail">
            {origin.map(|origin| view! {
                <div>
                    <dt>"Queued"</dt>
                    <dd>
                        {origin.when.map_or_else(|| "Time unknown".to_string(), |(time, _)| time)}
                        {match origin.queuer {
                            Some(name) => format!(" by {name}"),
                            None => ", queuer unknown".to_string(),
                        }}
                    </dd>
                </div>
            })}
            {queued.then(|| view! {
                <div><dt>"Queue timer"</dt><dd>{original_timer(&front)}</dd></div>
                <div><dt>"Queue ends"</dt><dd>{time(starts_at.map(|at| at - QUEUE_ENTRY_TO_START_SECS))}</dd></div>
            })}
            <div><dt>"War starts"</dt><dd>{time(starts_at)}</dd></div>
        </dl>
    }
}

/// Everything a member chip or roster row shows that does not re-read the board, so a
/// changed class or presence remounts it.
pub(super) fn member_key(member: &RosterMember) -> (String, Option<&'static str>, bool) {
    (
        member.username.clone(),
        member.class.map(PlayerClass::label),
        member.is_warring(),
    )
}

#[component]
fn TowerLine(tower: Option<Tower>, win_at: Option<i64>) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let win = move || win_at.map(|at| format!("win ~{}", format_clock(at - room.now.get())));
    match tower.filter(Tower::is_measured) {
        Some(tower) => {
            let percent = (tower.health.clamp(0.0, 1.0) * 100.0).round();
            let ehp = tower.remaining_ehp().map(format_stat);
            let dps = tower.dps.map(|dps| format_stat(dps as f64));
            view! {
                <div class="war-tower">
                    <div class="war-tower-bar" role="meter" aria-label="Tower health" aria-valuemin="0" aria-valuemax="100" aria-valuenow=percent.to_string()>
                        <span style:width=format!("{percent}%") />
                    </div>
                    <div class="war-tower-figures">
                        <span>{format!("{percent}%")}</span>
                        {ehp.map(|ehp| view! { <span><abbr title="Effective HP left">"EHP"</abbr>" "{ehp}</span> })}
                        {dps.map(|dps| view! { <span><abbr title="Tower damage per second">"DPS"</abbr>" "{dps}</span> })}
                        {move || win().map(|win| view! { <span>{win}</span> })}
                    </div>
                </div>
            }
            .into_any()
        }
        // Unmeasured figures are left out, not reported as missing.
        None => view! {
            {move || win().map(|win| view! { <div class="war-tower war-tower-unmeasured"><span>{win}</span></div> })}
        }
        .into_any(),
    }
}

/// A member's name with their class, opening their detail.
#[component]
pub(super) fn MemberChip(member: RosterMember) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let class = member.class;
    let warring = member.is_warring();
    let name = member.username.clone();
    let label = member_label(&member);
    view! {
        <button
            type="button"
            class="war-member"
            class:war-member-warring=warring
            aria-label=label
            on:click=move |event| room.open_player(&name, &event)
        >
            <ClassIcon class />
            <span class="war-member-name">{member.username}</span>
        </button>
    }
}

fn member_label(member: &RosterMember) -> String {
    match member.class {
        Some(class) => format!("{}, {}", member.username, class.label()),
        None => member.username.clone(),
    }
}

#[component]
pub(super) fn ClassIcon(class: Option<PlayerClass>) -> impl IntoView {
    match class.and_then(|class| class_icon_url(class.label()).map(|url| (url, class.label()))) {
        Some((url, label)) => view! {
            <img class="war-class-icon" src=url alt="" title=label width="14" height="14" />
        }
        .into_any(),
        // Keeps names aligned; an unknown class is never guessed.
        None => view! { <span class="war-class-icon war-class-unknown" aria-hidden="true" /> }
            .into_any(),
    }
}

#[component]
fn RosterSection() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let roster = Memo::new(move |_| {
        room.board.with(|board| {
            board
                .as_ref()
                .map(|board| board.roster.clone())
                .unwrap_or_default()
        })
    });
    let split = move |warring: bool| {
        roster.with(|roster| {
            roster
                .iter()
                .filter(|member| member.is_warring() == warring)
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let warring = Memo::new(move |_| split(true));
    let online = Memo::new(move |_| split(false));

    view! {
        <Show when=move || !roster.with(Vec::is_empty)>
            <section class="war-section war-section-roster scrollbar-thin" aria-label="Members">
                <RosterGroup title="Warring" stage="warring" members=warring />
                <RosterGroup title="Online" stage="online" members=online />
            </section>
        </Show>
    }
}

#[component]
fn RosterGroup(
    title: &'static str,
    stage: &'static str,
    members: Memo<Vec<RosterMember>>,
) -> impl IntoView {
    view! {
        <Show when=move || !members.with(Vec::is_empty)>
            <div class="war-group" data-stage=stage>
                <h4 class="war-group-title">
                    {title}
                    <span class="war-count">{move || members.with(Vec::len)}</span>
                </h4>
                <ul class="war-roster">
                    <For each=move || members.get() key=member_key let:member>
                        <RosterRow member />
                    </For>
                </ul>
            </div>
        </Show>
    }
}

#[component]
fn RosterRow(member: RosterMember) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let username = member.username.clone();
    let name = username.clone();
    // The location changes without remounting the row.
    let place = Memo::new(move |_| {
        room.member(&username)
            .as_ref()
            .map(whereabouts)
            .unwrap_or_default()
    });
    // Only a territory on the map is a place to go; "Outside territories" is not.
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let focus_map = MapFocus::from_context();
    let destination = {
        let username = member.username.clone();
        Memo::new(move |_| {
            let territory = room.member(&username)?.territory()?.to_string();
            territories
                .with(|map| map.contains_key(&territory))
                .then_some(territory)
        })
    };
    let label = member_label(&member);
    let warring = member.is_warring();
    view! {
        <li class="war-roster-row">
            <button type="button" class="war-roster-name" aria-label=label on:click=move |event| room.open_player(&name, &event)>
                <ClassIcon class=member.class />
                <span class="war-member-name">{member.username.clone()}</span>
            </button>
            {move || match destination.get() {
                Some(territory) => {
                    let focus = territory.clone();
                    let title = format!("Show {territory} on the map");
                    view! {
                        <button
                            type="button"
                            class="war-roster-place war-link"
                            class:war-roster-warring=warring
                            title=title
                            on:click=move |_| focus_map.territory(&focus)
                        >
                            {territory}
                        </button>
                    }
                    .into_any()
                }
                None => view! {
                    <span class="war-roster-place" class:war-roster-warring=warring>{move || place.get()}</span>
                }
                .into_any(),
            }}
        </li>
    }
}

/// The phone entry point: what is happening at a glance, opening the sheet.
#[component]
fn WarSummaryButton() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let counts = Memo::new(move |_| {
        room.board.with(|board| {
            board.as_ref().map(|board| {
                (
                    board.unavailable,
                    board.count(StageKind::Fighting),
                    board.count(StageKind::Entered) + board.count(StageKind::Queued),
                    board.roster.len(),
                )
            })
        })
    });
    let label = move || match counts.get() {
        Some((true, ..)) | None => "War: feed unavailable".to_string(),
        Some((false, fighting, pending, members)) => format!(
            "War: {fighting} fighting, {pending} queued or entered, {members} members reporting"
        ),
    };
    let count = move |pick: fn((bool, usize, usize, usize)) -> usize| counts.get().map_or(0, pick);
    view! {
        <button
            type="button"
            class="war-summary"
            class:war-summary-stale=move || room.stale.get() || counts.get().is_some_and(|counts| counts.0)
            aria-label=label
            aria-haspopup="dialog"
            aria-expanded=move || room.sheet_open.get().to_string()
            on:click=move |_| room.sheet_open.update(|open| *open = !*open)
        >
            <span class="war-summary-item" data-stage="fighting">
                <span class="war-glyph war-glyph-swords" aria-hidden="true" />
                {move || count(|counts| counts.1)}
            </span>
            <span class="war-summary-item" data-stage="queued">
                <span class="war-glyph war-glyph-hourglass" aria-hidden="true" />
                {move || count(|counts| counts.2)}
            </span>
            <span class="war-summary-item" data-stage="online">
                <span class="war-glyph war-glyph-dot" aria-hidden="true" />
                {move || count(|counts| counts.3)}
            </span>
        </button>
    }
}

/// The phone sheet: the panel's content, from the bottom, scrolled on its own.
#[component]
fn WarSheet() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let SidebarOpen(sidebar_open) = expect_context();
    let PeekTerritory(peek) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let sheet_ref = NodeRef::<leptos::html::Div>::new();
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);

    // On a phone the territory sheet and card share the bottom edge with this one; in a
    // short landscape window the sidebar is a side panel and may stay open beside it.
    Effect::new(move || {
        if room.sheet_open.get() {
            peek.set(None);
            if is_mobile.get_untracked() {
                sidebar_open.set(false);
            }
        }
    });
    Effect::new(move || {
        if sidebar_open.get() && is_mobile.get_untracked() {
            room.sheet_open.set(false);
        }
    });
    Effect::new(move |was_open: Option<bool>| {
        let open = room.sheet_open.get();
        match (was_open.unwrap_or(false), open) {
            (false, true) => {
                opener.set_value(focused_element());
                if let Some(sheet) = sheet_ref.get() {
                    focus_next_frame(sheet.into());
                }
            }
            (true, false) => restore_focus(opener.get_value()),
            _ => {}
        }
        open
    });
    let close = Callback::new(move |()| room.sheet_open.set(false));
    // The sheet goes when the screen grows out of the compact layout or the panel is turned
    // off; an open flag left behind would keep the map's shortcuts switched off.
    on_cleanup(move || {
        room.sheet_open.try_set(false);
    });

    view! {
        <div class="war-sheet-scrim" class:war-sheet-scrim-visible=move || room.sheet_open.get() on:click=move |_| close.run(()) />
        <div
            class="war-sheet"
            class:war-sheet-open=move || room.sheet_open.get()
            role="dialog"
            aria-modal="true"
            aria-label="War"
            tabindex="-1"
            node_ref=sheet_ref
            inert=move || !room.sheet_open.get()
            on:keydown=move |event: web_sys::KeyboardEvent| match event.key().as_str() {
                "Escape" => {
                    event.prevent_default();
                    close.run(());
                }
                "Tab" => {
                    if let Some(sheet) = sheet_ref.get() {
                        trap_tab(&event, &sheet);
                    }
                }
                _ => {}
            }
        >
            <WarRoomHeader on_close=close />
            <div class="war-sheet-body">
                <WarRoomBody />
            </div>
        </div>
    }
}

/// Collapse control on the panel's right edge.
#[component]
fn WarPanelToggle(panel_open: RwSignal<bool>) -> impl IntoView {
    let label = move || {
        if panel_open.get() {
            "Hide war panel"
        } else {
            "Show war panel"
        }
    };
    view! {
        <button
            type="button"
            class="war-panel-toggle"
            aria-label=label
            aria-expanded=move || panel_open.get().to_string()
            title=label
            on:click=move |_| panel_open.update(|open| *open = !*open)
        >
            {move || if panel_open.get() { "\u{00AB}" } else { "\u{00BB}" }}
        </button>
    }
}

/// Drag the panel wider or narrower from its right edge, like the sidebar's own handle.
#[component]
fn WarPanelResizeHandle() -> impl IntoView {
    let WarPanelOpen(panel_open) = expect_context();
    let WarPanelWidth(panel_width) = expect_context();
    let drag = Rc::new(std::cell::Cell::new(None::<(i32, f64, f64)>));
    let dragging = RwSignal::new(false);

    let drag_down = drag.clone();
    let drag_move = drag.clone();
    let end_drag = move |event: web_sys::PointerEvent| {
        if drag
            .get()
            .is_none_or(|(pointer, ..)| pointer != event.pointer_id())
        {
            return;
        }
        drag.set(None);
        dragging.set(false);
        if let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
        {
            target.release_pointer_capture(event.pointer_id()).ok();
        }
    };
    let end_cancel = end_drag.clone();

    view! {
        <div
            class="war-panel-resize-handle"
            class:war-panel-resize-active=move || dragging.get()
            style:display=move || if panel_open.get() { "block" } else { "none" }
            on:pointerdown=move |event: web_sys::PointerEvent| {
                if !event.is_primary() || event.button() != 0 {
                    return;
                }
                event.prevent_default();
                dragging.set(true);
                drag_down.set(Some((
                    event.pointer_id(),
                    f64::from(event.client_x()),
                    panel_width.get_untracked(),
                )));
                if let Some(target) = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    target.set_pointer_capture(event.pointer_id()).ok();
                }
            }
            on:pointermove=move |event: web_sys::PointerEvent| {
                let Some((pointer, start_x, start_width)) = drag_move.get() else {
                    return;
                };
                if pointer != event.pointer_id() {
                    return;
                }
                event.prevent_default();
                panel_width.set(clamp_war_panel_width(
                    start_width + f64::from(event.client_x()) - start_x,
                ));
            }
            on:pointerup=end_drag
            on:pointercancel=end_cancel
        />
    }
}
