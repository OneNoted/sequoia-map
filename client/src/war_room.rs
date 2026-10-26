//! The war room: Sequoia's live war situation for members, on the map and in the HUD.
//!
//! [`WarRoom`] turns the war controller feed into one [`WarBoard`] and owns what the client
//! adds to it: recent outcomes, staleness, the clock and the open player detail. The map's
//! borders and plates and every HUD row read that same board, so they cannot disagree.
//!
//! Members only, live mode only: the server refuses `/api/warcontroller` and withholds the SSE
//! `warcontroller` frames from everyone else, and [`WarRoom::board`] is `None` unless the
//! viewer is a member looking at the live map.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use sequoia_browser_map::BrowserMap;
use sequoia_browser_map::live_feed::ConnectionStatus;
use sequoia_browser_map::overlay::{PlateLabel, WarPlate, WarPlates};
use sequoia_shared::tower::format_stat;
use sequoia_shared::war_board::{
    FrontClock, FrontStage, Outcome, OutcomeKind, Presence, RosterMember, StageKind, WarBoard,
    WarFront, format_clock, outcomes,
};
use sequoia_shared::{TreasuryLevel, WarControllerState};
use wasm_bindgen::JsCast;

use crate::app::{
    DetailReturnGuild, IsMobile, PeekTerritory, Selected, ShowSettings, ShowWarMarkers,
    ShowWarPanel, SidebarOpen, SidebarTransient, WarPanelOpen, WarPanelWidth,
    clamp_war_panel_width,
};
use crate::territory::{ClientTerritoryMap, is_sequoia_guild};
use crate::ui_icons::class_icon_url;

const WARCONTROLLER_ENDPOINT: &str = "/api/warcontroller";
/// How long a cancelled queue or an ended war stays on the map and in the panel.
const OUTCOME_SECS: i64 = 90;
/// World units kept around a member's position when showing them on the map.
const POSITION_CONTEXT_WORLD: f64 = 180.0;
/// At or below this viewport height (CSS pixels) the war panel would crowd the map, so the
/// HUD uses the phone's summary button and drawer, as `input.css` does for the drawer.
const COMPACT_HUD_MAX_HEIGHT: f64 = 520.0;

pub async fn fetch_warcontroller() -> Result<WarControllerState, String> {
    let response = gloo_net::http::Request::get(WARCONTROLLER_ENDPOINT)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if !response.ok() {
        return Err(format!("status {}", response.status()));
    }
    response
        .json::<WarControllerState>()
        .await
        .map_err(|error| format!("decode failed: {error}"))
}

/// The war situation and the client state around it. Cheap to copy; provided as context.
#[derive(Clone, Copy)]
pub(crate) struct WarRoom {
    /// The board while a member is looking at the live map; `None` otherwise.
    pub board: Memo<Option<Arc<WarBoard>>>,
    /// Fronts that recently left the board.
    pub outcomes: RwSignal<Vec<Outcome>>,
    /// The live connection is down, so what is shown may be out of date.
    pub stale: Memo<bool>,
    /// Seconds the clocks count against: the browser clock, never behind the feed snapshot.
    pub now: Memo<i64>,
    /// The member whose detail is open.
    pub player: RwSignal<Option<String>>,
    /// The phone sheet is open.
    pub sheet_open: RwSignal<bool>,
}

pub(crate) struct WarRoomInputs {
    pub feed: RwSignal<Option<WarControllerState>>,
    /// Member viewer on the live map.
    pub visible: Signal<bool>,
    pub territories: RwSignal<ClientTerritoryMap>,
    pub connection: RwSignal<ConnectionStatus>,
    pub tick: RwSignal<i64>,
}

impl WarRoom {
    pub fn new(inputs: WarRoomInputs) -> Self {
        let WarRoomInputs {
            feed,
            visible,
            territories,
            connection,
            tick,
        } = inputs;
        let board = Memo::new(move |_| {
            if !visible.get() {
                return None;
            }
            feed.with(|state| {
                let state = state.as_ref()?;
                let board =
                    territories.with(|map| WarBoard::new(state, |x, z| territory_at(map, x, z)));
                Some(Arc::new(board))
            })
        });
        let now = Memo::new(move |_| {
            let snapshot = feed.with(|state| state.as_ref().map_or(0, |state| state.timestamp));
            tick.get().max(snapshot)
        });
        let outcome_list = RwSignal::new(Vec::<Outcome>::new());
        Effect::new(move |previous: Option<Option<Arc<WarBoard>>>| {
            let next = board.get();
            match (previous.flatten(), next.as_ref()) {
                (Some(previous), Some(next)) => {
                    let left = outcomes(&previous, next, now.get_untracked());
                    outcome_list.maybe_update(|list| {
                        let before = list.len();
                        // A territory back on the board is no longer an outcome.
                        list.retain(|outcome| next.front(&outcome.territory).is_none());
                        let changed = list.len() != before || !left.is_empty();
                        list.extend(left);
                        changed
                    });
                }
                (_, None) => outcome_list.maybe_update(|list| {
                    let changed = !list.is_empty();
                    list.clear();
                    changed
                }),
                (None, Some(_)) => {}
            }
            next
        });
        Effect::new(move || {
            let now = now.get();
            outcome_list.maybe_update(|list| {
                let before = list.len();
                list.retain(|outcome| now - outcome.at < OUTCOME_SECS);
                list.len() != before
            });
        });
        let player = RwSignal::new(None::<String>);
        let sheet_open = RwSignal::new(false);
        Effect::new(move || {
            if board.with(Option::is_none) {
                player.set(None);
                sheet_open.set(false);
            }
        });
        Self {
            board,
            outcomes: outcome_list,
            stale: Memo::new(move |_| connection.get() != ConnectionStatus::Live),
            now,
            player,
            sheet_open,
        }
    }

    /// Each front's stage, for the map's borders. Changes only when a stage does.
    pub fn stages(&self) -> Memo<HashMap<String, StageKind>> {
        let board = self.board;
        Memo::new(move |_| {
            board.with(|board| {
                board.as_ref().map_or_else(HashMap::new, |board| {
                    board
                        .fronts
                        .iter()
                        .map(|front| (front.territory.clone(), front.stage_kind()))
                        .collect()
                })
            })
        })
    }

    pub fn front(&self, territory: &str) -> Option<WarFront> {
        self.board
            .with(|board| board.as_ref()?.front(territory).cloned())
    }
}

/// The territory containing a world position. Territories do not overlap in practice; if
/// two ever did, the smaller one wins, then the name, so the answer is stable.
pub(crate) fn territory_at(map: &ClientTerritoryMap, x: f64, z: f64) -> Option<String> {
    map.iter()
        .filter(|(_, ct)| {
            let region = &ct.territory.location;
            (f64::from(region.left())..=f64::from(region.right())).contains(&x)
                && (f64::from(region.top())..=f64::from(region.bottom())).contains(&z)
        })
        .min_by_key(|(name, ct)| {
            let region = &ct.territory.location;
            (
                i64::from(region.width()) * i64::from(region.height()),
                *name,
            )
        })
        .map(|(name, _)| name.clone())
}

/// The plates the map draws: one per front and per recent outcome, anchored on the
/// territory's top edge, fighting drawn last so it lands on top.
pub(crate) fn war_plates(
    board: &WarBoard,
    recent: &[Outcome],
    now: i64,
    stale: bool,
    territories: &ClientTerritoryMap,
) -> Option<WarPlates> {
    let anchor = |territory: &str| {
        let region = &territories.get(territory)?.territory.location;
        Some([region.midpoint_x() as f32, region.top() as f32])
    };
    let mut plates: Vec<WarPlate> = recent
        .iter()
        .filter_map(|outcome| {
            Some(WarPlate {
                world: anchor(&outcome.territory)?,
                label: outcome_label(outcome, territories),
            })
        })
        .collect();
    plates.extend(board.fronts.iter().rev().filter_map(|front| {
        Some(WarPlate {
            world: anchor(&front.territory)?,
            label: front_label(front, now),
        })
    }));
    (!plates.is_empty()).then_some(WarPlates { plates, stale })
}

fn front_label(front: &WarFront, now: i64) -> PlateLabel {
    let entered = front.stage_kind() == StageKind::Entered;
    match front.clock(now) {
        FrontClock::StartsIn(seconds) => PlateLabel::StartsIn { entered, seconds },
        FrontClock::Due => PlateLabel::Due { entered },
        FrontClock::Unscheduled => PlateLabel::Waiting { entered },
        FrontClock::FightingFor(seconds) => PlateLabel::Fighting {
            seconds: Some(seconds),
            health: measured_health(front),
        },
        FrontClock::Fighting => PlateLabel::Fighting {
            seconds: None,
            health: measured_health(front),
        },
    }
}

fn measured_health(front: &WarFront) -> Option<f32> {
    match front.stage {
        FrontStage::Fighting {
            tower: Some(tower), ..
        } if tower.is_measured() => Some(tower.health),
        _ => None,
    }
}

/// A war that ended reads as captured once the territory shows as ours. Ownership arrives
/// from a different feed, so an ended war can turn into a capture a few seconds later.
fn outcome_label(outcome: &Outcome, territories: &ClientTerritoryMap) -> PlateLabel {
    match outcome.kind {
        OutcomeKind::Cancelled => PlateLabel::Cancelled,
        OutcomeKind::WarEnded if owned_by_sequoia(&outcome.territory, territories) => {
            PlateLabel::Captured
        }
        OutcomeKind::WarEnded => PlateLabel::Ended,
    }
}

fn owned_by_sequoia(territory: &str, territories: &ClientTerritoryMap) -> bool {
    territories
        .get(territory)
        .is_some_and(|ct| is_sequoia_guild(&ct.territory.guild.name, &ct.territory.guild.prefix))
}

/// Publishes the plates to the map every second while there is anything to count.
#[component]
pub(crate) fn WarPlatesLayer() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let ShowWarMarkers(show_markers) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let map = expect_context::<BrowserMap>();
    Effect::new(move || {
        let plates = show_markers
            .get()
            .then(|| {
                room.board.with(|board| {
                    let board = board.as_ref()?;
                    room.outcomes.with(|recent| {
                        territories.with(|map| {
                            war_plates(board, recent, room.now.get(), room.stale.get(), map)
                        })
                    })
                })
            })
            .flatten();
        map.war_plates().maybe_update(|current| {
            if current.is_none() && plates.is_none() {
                return false;
            }
            *current = plates.map(Arc::new);
            true
        });
    });
    on_cleanup(move || {
        map.war_plates().try_set(None);
    });
}

/// Square colour for a Wynncraft difficulty tier, light green through dark red. Deliberately
/// not [`TreasuryLevel::color_rgb`], the treasury palette, where High is green.
pub(crate) fn difficulty_color(raw: Option<&str>) -> &'static str {
    match raw.and_then(TreasuryLevel::from_api_tier) {
        Some(TreasuryLevel::VeryLow) => "#8ce99a",
        Some(TreasuryLevel::Low) => "#50c878",
        Some(TreasuryLevel::Medium) => "#f5c542",
        Some(TreasuryLevel::High) => "#ff4545",
        Some(TreasuryLevel::VeryHigh) => "#8b1a1a",
        None => "#6f748f",
    }
}

fn difficulty_label(raw: Option<&str>) -> Option<&'static str> {
    raw.and_then(TreasuryLevel::from_api_tier)
        .map(TreasuryLevel::label)
}

/// A front's clock as (visible text, spoken text).
pub(crate) fn clock_text(clock: FrontClock) -> (String, String) {
    match clock {
        FrontClock::StartsIn(seconds) => {
            let time = format_clock(seconds);
            (time.clone(), format!("starts in {time}"))
        }
        FrontClock::Due => ("Due".into(), "start due, not yet confirmed".into()),
        FrontClock::Unscheduled => ("Time unknown".into(), "start time unknown".into()),
        FrontClock::FightingFor(seconds) => {
            let time = format_clock(seconds);
            (time.clone(), format!("fighting for {time}"))
        }
        FrontClock::Fighting => ("War".into(), "fighting".into()),
    }
}

/// Where a member is, in words.
pub(crate) fn whereabouts(member: &RosterMember) -> String {
    match &member.presence {
        Presence::Warring { territory } => territory.clone(),
        Presence::Online {
            territory: Some(territory),
            ..
        } => territory.clone(),
        Presence::Online {
            position: Some(_), ..
        } => "Outside territories".into(),
        Presence::Online { position: None, .. } => "Location unknown".into(),
    }
}

/// Moves the map to what a war row points at. Built from context while a component is set
/// up, so its methods can run from event handlers.
#[derive(Clone, Copy)]
pub(crate) struct MapFocus {
    territories: RwSignal<ClientTerritoryMap>,
    map: BrowserMap,
    room: WarRoom,
    is_mobile: RwSignal<bool>,
    selected: RwSignal<Option<String>>,
    peek: RwSignal<Option<String>>,
    detail_return_guild: RwSignal<Option<String>>,
    show_settings: RwSignal<bool>,
    sidebar_open: RwSignal<bool>,
    sidebar_transient: RwSignal<bool>,
}

impl MapFocus {
    pub fn from_context() -> Self {
        let IsMobile(is_mobile) = expect_context();
        let Selected(selected) = expect_context();
        let PeekTerritory(peek) = expect_context();
        let DetailReturnGuild(detail_return_guild) = expect_context();
        let ShowSettings(show_settings) = expect_context();
        let SidebarOpen(sidebar_open) = expect_context();
        let SidebarTransient(sidebar_transient) = expect_context();
        Self {
            territories: expect_context(),
            map: expect_context(),
            room: expect_context(),
            is_mobile,
            selected,
            peek,
            detail_return_guild,
            show_settings,
            sidebar_open,
            sidebar_transient,
        }
    }

    /// Brings a territory into view and opens it the way a tap on the map would: its card
    /// on a phone, the sidebar detail elsewhere.
    pub fn territory(&self, name: &str) {
        if let Some(region) = self
            .territories
            .with_untracked(|map| map.get(name).map(|ct| ct.territory.location.clone()))
        {
            self.map.camera().focus(&region);
        }
        self.detail_return_guild.set(None);
        self.room.sheet_open.set(false);
        if self.is_mobile.get_untracked() {
            self.selected.set(None);
            self.peek.set(Some(name.to_string()));
            return;
        }
        self.show_settings.set(false);
        if !self.sidebar_open.get_untracked() {
            self.sidebar_open.set(true);
            self.sidebar_transient.set(true);
        }
        self.selected.set(Some(name.to_string()));
    }

    /// Centres the map on a world position with some surroundings.
    pub fn position(&self, (x, z): (f64, f64)) {
        self.room.sheet_open.set(false);
        self.map.camera().fit((
            x - POSITION_CONTEXT_WORLD,
            z - POSITION_CONTEXT_WORLD,
            x + POSITION_CONTEXT_WORLD,
            z + POSITION_CONTEXT_WORLD,
        ));
    }
}

/// The war HUD: a left panel on roomy screens; a summary button and a sheet (a drawer on
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
        <crate::player_detail::PlayerDetail />
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
    let groups = Memo::new(move |_| {
        room.board.with(|board| {
            let Some(board) = board.as_ref() else {
                return Vec::new();
            };
            [StageKind::Fighting, StageKind::Entered, StageKind::Queued]
                .into_iter()
                .filter_map(|kind| {
                    let names: Vec<String> = board
                        .fronts
                        .iter()
                        .filter(|front| front.stage_kind() == kind)
                        .map(|front| front.territory.clone())
                        .collect();
                    (!names.is_empty()).then_some((kind, names))
                })
                .collect::<Vec<_>>()
        })
    });
    let recent = Memo::new(move |_| {
        room.outcomes
            .with(|list| list.iter().rev().cloned().collect::<Vec<_>>())
    });

    view! {
        <section class="war-section" aria-labelledby="war-fronts-heading">
            <h3 class="war-section-title" id="war-fronts-heading">"Territories"</h3>
            <Show when=move || !recent.with(Vec::is_empty)>
                <ul class="war-outcomes">
                    <For each=move || recent.get() key=|outcome| (outcome.territory.clone(), outcome.at) let:outcome>
                        <OutcomeItem outcome />
                    </For>
                </ul>
            </Show>
            <Show
                when=move || !groups.with(Vec::is_empty)
                fallback=|| view! { <p class="war-empty">"No territories queued"</p> }
            >
                <For each=move || groups.get() key=|(kind, names)| (*kind, names.clone()) let:group>
                    <div class="war-group" data-stage=stage_slug(group.0)>
                        <h4 class="war-group-title">
                            {group.0.label()}
                            <span class="war-count">{group.1.len()}</span>
                        </h4>
                        <ol class="war-fronts">
                            {group.1.into_iter().map(|territory| view! { <FrontItem territory /> }).collect_view()}
                        </ol>
                    </div>
                </For>
            </Show>
        </section>
        <RosterSection />
    }
}

pub(crate) fn stage_slug(kind: StageKind) -> &'static str {
    match kind {
        StageKind::Fighting => "fighting",
        StageKind::Entered => "entered",
        StageKind::Queued => "queued",
    }
}

#[component]
fn OutcomeItem(outcome: Outcome) -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let focus_map = MapFocus::from_context();
    let territory = outcome.territory.clone();
    let label = Memo::new(move |_| {
        territories.with(|map| match outcome_label(&outcome, map) {
            PlateLabel::Captured => ("captured", "Captured"),
            PlateLabel::Cancelled => ("cancelled", "Queue cancelled"),
            _ => ("ended", "War ended"),
        })
    });
    let focus = territory.clone();
    view! {
        <li class="war-outcome" data-outcome=move || label.get().0>
            <button type="button" class="war-link" on:click=move |_| focus_map.territory(&focus)>
                {territory}
            </button>
            <span class="war-outcome-label">{move || label.get().1}</span>
        </li>
    }
}

/// One territory's row. Keyed by name in its group; reads its front from the board so it
/// stays current between regroupings.
#[component]
fn FrontItem(territory: String) -> impl IntoView {
    let room = expect_context::<WarRoom>();
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
        Memo::new(move |_| {
            room.board.with(|board| {
                board.as_ref().map_or_else(Vec::new, |board| {
                    board.members_in(&territory).cloned().collect::<Vec<_>>()
                })
            })
        })
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
    let focus_map = MapFocus::from_context();
    let focus = territory.clone();
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
            </div>
            {move || tower.get().map(|(tower, win_at)| view! { <TowerLine tower win_at /> })}
            <Show
                when=move || !members.with(Vec::is_empty)
                fallback=|| view! { <p class="war-front-empty">"Nobody inside"</p> }
            >
                <ul class="war-front-members" aria-label="Members inside">
                    <For each=move || members.get() key=|member| member.username.clone() let:member>
                        <li><MemberChip member /></li>
                    </For>
                </ul>
            </Show>
        </li>
    }
}

#[component]
fn TowerLine(
    tower: Option<sequoia_shared::war_board::Tower>,
    win_at: Option<i64>,
) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let measured = tower.filter(|tower| tower.is_measured());
    let win = move || win_at.map(|at| format!("win ~{}", format_clock(at - room.now.get())));
    match measured {
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
        None => view! {
            <div class="war-tower war-tower-unmeasured">
                <span>"Tower not measured yet"</span>
                {move || win().map(|win| view! { <span>{win}</span> })}
            </div>
        }
        .into_any(),
    }
}

/// A member's name with their class, opening their detail.
#[component]
pub(crate) fn MemberChip(member: RosterMember) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let class = member.class;
    let warring = member.is_warring();
    let name = member.username.clone();
    let label = match class {
        Some(class) => format!("{}, {}", member.username, class.label()),
        None => member.username.clone(),
    };
    view! {
        <button
            type="button"
            class="war-member"
            class:war-member-warring=warring
            aria-label=label
            on:click=move |_| room.player.set(Some(name.clone()))
        >
            <ClassIcon class />
            <span class="war-member-name">{member.username}</span>
        </button>
    }
}

#[component]
pub(crate) fn ClassIcon(class: Option<sequoia_shared::PlayerClass>) -> impl IntoView {
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
        <section class="war-section" aria-labelledby="war-members-heading">
            <h3 class="war-section-title" id="war-members-heading">"Members"</h3>
            <Show
                when=move || !roster.with(Vec::is_empty)
                fallback=|| view! { <p class="war-empty">"No members reporting"</p> }
            >
                <RosterGroup title="Warring" stage="warring" members=warring />
                <RosterGroup title="Online" stage="online" members=online />
            </Show>
        </section>
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
                    <For each=move || members.get() key=|member| member.username.clone() let:member>
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
    let class = member.class;
    let warring = member.is_warring();
    // Re-read so the location follows the member between feed updates.
    let place = Memo::new(move |_| {
        room.board.with(|board| {
            board
                .as_ref()
                .and_then(|board| board.member(&username))
                .map(whereabouts)
                .unwrap_or_default()
        })
    });
    let label = match class {
        Some(class) => format!("{}, {}", member.username, class.label()),
        None => member.username.clone(),
    };
    view! {
        <li class="war-roster-row">
            <button type="button" class="war-roster-name" aria-label=label on:click=move |_| room.player.set(Some(name.clone()))>
                <ClassIcon class />
                <span class="war-member-name">{member.username}</span>
            </button>
            <span class="war-roster-place" class:war-roster-warring=warring>{move || place.get()}</span>
        </li>
    }
}

/// The phone entry point: what is happening at a glance, opening the sheet.
#[component]
fn WarSummaryButton() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let counts = Memo::new(move |_| {
        room.board.with(|board| {
            board.as_ref().map_or((0, 0, 0), |board| {
                (
                    board.count(StageKind::Fighting),
                    board.count(StageKind::Entered) + board.count(StageKind::Queued),
                    board.roster.len(),
                )
            })
        })
    });
    let label = move || {
        let (fighting, pending, members) = counts.get();
        format!("War: {fighting} fighting, {pending} queued, {members} members reporting")
    };
    view! {
        <button
            type="button"
            class="war-summary"
            class:war-summary-stale=move || room.stale.get()
            aria-label=label
            aria-haspopup="dialog"
            aria-expanded=move || room.sheet_open.get().to_string()
            on:click=move |_| room.sheet_open.update(|open| *open = !*open)
        >
            <span class="war-summary-item" data-stage="fighting">
                <span class="war-glyph war-glyph-swords" aria-hidden="true" />
                {move || counts.get().0}
            </span>
            <span class="war-summary-item" data-stage="queued">
                <span class="war-glyph war-glyph-hourglass" aria-hidden="true" />
                {move || counts.get().1}
            </span>
            <span class="war-summary-item" data-stage="online">
                <span class="war-glyph war-glyph-dot" aria-hidden="true" />
                {move || counts.get().2}
            </span>
        </button>
    }
}

/// The phone sheet: the panel's content, from the bottom, scrolled on its own.
#[component]
fn WarSheet() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let SidebarOpen(sidebar_open) = expect_context();
    // One sheet at a time: the war sheet and the territory sheet share the bottom edge.
    Effect::new(move || {
        if room.sheet_open.get() {
            sidebar_open.set(false);
        }
    });
    Effect::new(move || {
        if sidebar_open.get() {
            room.sheet_open.set(false);
        }
    });
    let close = Callback::new(move |()| room.sheet_open.set(false));
    let sheet_ref = NodeRef::<leptos::html::Div>::new();
    Effect::new(move || {
        if room.sheet_open.get()
            && let Some(sheet) = sheet_ref.get()
        {
            let _ = sheet.focus();
        }
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
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Escape" {
                    close.run(());
                }
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
    view! {
        <button
            type="button"
            class="war-panel-toggle"
            aria-label=move || if panel_open.get() { "Hide war panel" } else { "Show war panel" }
            aria-expanded=move || panel_open.get().to_string()
            title=move || if panel_open.get() { "Hide war panel" } else { "Show war panel" }
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sequoia_shared::war_board::{FrontStage, Tower};
    use sequoia_shared::{GuildRef, Region, Territory};

    use super::*;
    use crate::territory::ClientTerritory;

    fn territory(
        name: &str,
        guild: (&str, &str),
        start: [i32; 2],
        end: [i32; 2],
    ) -> (String, ClientTerritory) {
        let territory = Territory {
            guild: GuildRef {
                uuid: "0".to_string(),
                name: guild.0.to_string(),
                prefix: guild.1.to_string(),
                color: None,
            },
            acquired: Utc::now(),
            location: Region { start, end },
            resources: Default::default(),
            connections: Vec::new(),
            runtime: None,
        };
        (
            name.to_string(),
            ClientTerritory::from_territory(name, territory),
        )
    }

    fn map() -> ClientTerritoryMap {
        [
            territory("Ragni", ("Avicia", "AVO"), [0, 0], [100, 100]),
            territory("Detlas", ("Sequoia", "SEQ"), [200, 0], [300, 100]),
        ]
        .into_iter()
        .collect()
    }

    fn front(territory: &str, stage: FrontStage) -> WarFront {
        WarFront {
            territory: territory.to_string(),
            difficulty: None,
            stage,
        }
    }

    #[test]
    fn a_position_is_placed_in_the_territory_containing_it() {
        let map = map();
        assert_eq!(territory_at(&map, 50.0, 50.0).as_deref(), Some("Ragni"));
        assert_eq!(territory_at(&map, 100.0, 100.0).as_deref(), Some("Ragni"));
        assert_eq!(territory_at(&map, 150.0, 50.0), None);
    }

    #[test]
    fn plates_sit_on_each_war_territory_top_edge_with_its_clock() {
        let board = WarBoard {
            fronts: vec![
                front(
                    "Ragni",
                    FrontStage::Fighting {
                        started_at: Some(940),
                        tower: Some(Tower {
                            health: 0.5,
                            ehp: Some(10),
                            dps: Some(2),
                        }),
                        expected_win_at: None,
                    },
                ),
                front(
                    "Detlas",
                    FrontStage::Queued {
                        starts_at: Some(1_128),
                    },
                ),
            ],
            roster: Vec::new(),
        };
        let plates = war_plates(&board, &[], 1_000, false, &map()).expect("plates");
        // Drawn last, on top: the fighting front.
        assert_eq!(plates.plates[1].world, [50.0, 0.0]);
        assert_eq!(
            plates.plates[1].label,
            PlateLabel::Fighting {
                seconds: Some(60),
                health: Some(0.5)
            }
        );
        assert_eq!(
            plates.plates[0].label,
            PlateLabel::StartsIn {
                entered: false,
                seconds: 128
            }
        );
    }

    #[test]
    fn an_unmeasured_tower_draws_no_health_bar() {
        let board = WarBoard {
            fronts: vec![front(
                "Ragni",
                FrontStage::Fighting {
                    started_at: None,
                    tower: Some(Tower {
                        health: 0.9,
                        ehp: None,
                        dps: None,
                    }),
                    expected_win_at: None,
                },
            )],
            roster: Vec::new(),
        };
        let plates = war_plates(&board, &[], 1_000, true, &map()).expect("plates");
        assert_eq!(
            plates.plates[0].label,
            PlateLabel::Fighting {
                seconds: None,
                health: None
            }
        );
        assert!(plates.stale);
    }

    #[test]
    fn an_ended_war_reads_as_captured_only_once_the_territory_is_ours() {
        let map = map();
        let ended = |territory: &str| Outcome {
            territory: territory.to_string(),
            kind: OutcomeKind::WarEnded,
            at: 1_000,
        };
        assert_eq!(outcome_label(&ended("Detlas"), &map), PlateLabel::Captured);
        assert_eq!(outcome_label(&ended("Ragni"), &map), PlateLabel::Ended);
    }

    #[test]
    fn no_fronts_and_no_outcomes_draw_no_plates() {
        let board = WarBoard {
            fronts: Vec::new(),
            roster: Vec::new(),
        };
        assert_eq!(war_plates(&board, &[], 1_000, false, &map()), None);
    }

    #[test]
    fn a_due_start_is_never_spoken_as_a_war() {
        assert_eq!(clock_text(FrontClock::Due).0, "Due");
        assert_eq!(clock_text(FrontClock::StartsIn(65)).1, "starts in 1:05");
        assert_eq!(
            clock_text(FrontClock::FightingFor(65)).1,
            "fighting for 1:05"
        );
    }

    #[test]
    fn whereabouts_never_invents_a_location() {
        let member = |presence| RosterMember {
            username: "x".into(),
            class: None,
            presence,
        };
        assert_eq!(
            whereabouts(&member(Presence::Warring {
                territory: "Ragni".into()
            })),
            "Ragni"
        );
        assert_eq!(
            whereabouts(&member(Presence::Online {
                position: Some((1.0, 1.0)),
                territory: None
            })),
            "Outside territories"
        );
        assert_eq!(
            whereabouts(&member(Presence::Online {
                position: None,
                territory: None
            })),
            "Location unknown"
        );
    }

    #[test]
    fn every_difficulty_tier_has_its_own_colour_and_unknown_is_neutral() {
        let colours = ["VERY_LOW", "LOW", "MEDIUM", "HIGH", "VERY_HIGH"]
            .map(|tier| difficulty_color(Some(tier)));
        let unique: std::collections::HashSet<_> = colours.iter().collect();
        assert_eq!(unique.len(), colours.len());
        assert_eq!(difficulty_color(Some("EXTREME")), "#6f748f");
        assert_eq!(difficulty_color(None), "#6f748f");
    }
}
