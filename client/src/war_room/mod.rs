//! The war room: Sequoia's live war situation for members, on the map and in the HUD.
//!
//! [`WarRoom`] turns the war controller feed into one [`WarBoard`] and owns what the client
//! adds to it: recent outcomes, staleness, the clock and the open player detail. The map's
//! borders and plates and every HUD row read that same board, so they cannot disagree.
//!
//! Members only, live mode only: the server refuses `/api/warcontroller` and withholds the SSE
//! `warcontroller` frames from everyone else, and [`WarRoom::board`] is `None` unless the
//! viewer is a member looking at the live map.

mod hud;
mod modal;
mod plates;
mod player;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use leptos::prelude::*;
use sequoia_browser_map::BrowserMap;
use sequoia_browser_map::live_feed::ConnectionStatus;
use sequoia_shared::war_board::{
    Ending, FrontClock, Outcome, Presence, RosterMember, StageKind, WarBoard, WarFront,
    format_clock, outcomes,
};
use sequoia_shared::{TreasuryLevel, WarControllerState};

use crate::app::{
    DetailReturnGuild, IsMobile, PeekTerritory, Selected, ShowSettings, SidebarOpen,
    SidebarTransient,
};
use crate::territory::{ClientTerritoryMap, is_sequoia_guild};

pub(crate) use hud::WarRoomHud;

const WARCONTROLLER_ENDPOINT: &str = "/api/warcontroller";
/// How long a cancelled queue or an ended war stays on the map and in the panel.
const OUTCOME_SECS: i64 = 90;
/// World units kept around a member's position when showing them on the map.
const POSITION_CONTEXT_WORLD: f64 = 180.0;
/// Recent frames the clock offset is estimated from.
const CLOCK_SAMPLES: usize = 8;

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
    /// The live connection dropped, so what is shown may be out of date.
    pub stale: Memo<bool>,
    /// The feed's clock: the browser's, moved forward by however far it trails the feed.
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

        // A frame is stamped when the backend built it and arrives a moment later, so the
        // stamp minus the arrival time can only understate how far the browser trails the
        // feed. Seeds repeat old stamps and read far below the truth, which is why the
        // largest recent sample is the estimate, and why a browser running ahead is left
        // alone rather than wound back on that evidence. For the same reason samples do not
        // age out: after a quiet spell the next frame is often a seed, and the good samples
        // before it are what keep the estimate.
        let samples = StoredValue::new(VecDeque::<i64>::with_capacity(CLOCK_SAMPLES));
        let behind = RwSignal::new(0i64);
        Effect::new(move || {
            let Some(stamp) = feed.with(|state| {
                state
                    .as_ref()
                    .filter(|state| !state.unavailable)
                    .map(|state| state.timestamp)
            }) else {
                return;
            };
            let arrived = chrono::Utc::now().timestamp();
            samples.update_value(|samples| {
                if samples.len() == CLOCK_SAMPLES {
                    samples.pop_front();
                }
                samples.push_back(stamp - arrived);
            });
            let estimate = samples.with_value(|samples| samples.iter().copied().max());
            behind.set(estimate.unwrap_or(0).max(0));
        });
        let now = Memo::new(move |_| tick.get() + behind.get());

        let outcome_list = RwSignal::new(Vec::<Outcome>::new());
        Effect::new(move |previous: Option<Option<Arc<WarBoard>>>| {
            let next = board.get();
            match (previous.flatten(), next.as_ref()) {
                (Some(previous), Some(next)) => {
                    // Judged on the feed's clock, shown for a while on the page's.
                    let shown_at = now.get_untracked();
                    let left = outcomes(&previous, next)
                        .into_iter()
                        .map(|outcome| Outcome {
                            at: shown_at,
                            ..outcome
                        });
                    outcome_list.maybe_update(|list| {
                        let before = list.len();
                        // A territory back on the board is no longer an outcome.
                        list.retain(|outcome| next.front(&outcome.territory).is_none());
                        let count = list.len();
                        list.extend(left);
                        list.len() != before || list.len() != count
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
            stale: Memo::new(move |_| connection.get() == ConnectionStatus::Reconnecting),
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

    /// Whether a war sheet or player detail is open and owns the keyboard.
    pub fn modal_open(&self) -> bool {
        self.player.with_untracked(Option::is_some) || self.sheet_open.get_untracked()
    }

    /// A key pressed outside the open war modal, which happens when a feed update removed
    /// the row that held focus: Escape still closes the modal on top, and Tab puts focus back
    /// inside it. Keys pressed inside a modal are its own. Returns whether the key was taken.
    pub fn handle_stray_key(&self, event: &web_sys::KeyboardEvent) -> bool {
        if !self.modal_open() {
            return false;
        }
        if modal::inside_open_modal(event.target()) {
            return true;
        }
        match event.key().as_str() {
            "Escape" => {
                event.prevent_default();
                if self.player.with_untracked(Option::is_some) {
                    self.player.set(None);
                } else {
                    self.sheet_open.set(false);
                }
            }
            "Tab" => {
                event.prevent_default();
                modal::focus_open_modal();
            }
            _ => {}
        }
        true
    }

    pub fn front(&self, territory: &str) -> Option<WarFront> {
        self.board
            .with(|board| board.as_ref()?.front(territory).cloned())
    }

    pub fn member(&self, username: &str) -> Option<RosterMember> {
        self.board
            .with(|board| board.as_ref()?.member(username).cloned())
    }

    pub fn members_in(&self, territory: &str) -> Vec<RosterMember> {
        self.board.with(|board| {
            board.as_ref().map_or_else(Vec::new, |board| {
                board.members_in(territory).cloned().collect()
            })
        })
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

/// How an outcome reads now, from who holds the territory.
pub(crate) fn ending(outcome: &Outcome, territories: &ClientTerritoryMap) -> Ending {
    let owned = territories
        .get(&outcome.territory)
        .is_some_and(|ct| is_sequoia_guild(&ct.territory.guild.name, &ct.territory.guild.prefix));
    outcome.ending(owned)
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

pub(crate) fn difficulty_label(raw: Option<&str>) -> Option<&'static str> {
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

/// A front's stage with its clock, for a one-line summary: "Queued 2:08", "Fighting 0:47",
/// "Entered, due", or the stage alone when there is no time to show.
pub(crate) fn stage_with_clock(front: &WarFront, now: i64) -> (StageKind, String) {
    let stage = front.stage_kind();
    let text = match front.clock(now) {
        FrontClock::StartsIn(seconds) | FrontClock::FightingFor(seconds) => {
            format!("{} {}", stage.label(), format_clock(seconds))
        }
        FrontClock::Due => format!("{}, due", stage.label()),
        FrontClock::Unscheduled | FrontClock::Fighting => stage.label().to_string(),
    };
    (stage, text)
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

pub(crate) fn stage_slug(kind: StageKind) -> &'static str {
    match kind {
        StageKind::Fighting => "fighting",
        StageKind::Entered => "entered",
        StageKind::Queued => "queued",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sequoia_shared::war_board::OutcomeKind;
    use sequoia_shared::{GuildRef, Region, Territory};

    use super::*;
    use crate::territory::ClientTerritory;

    pub(super) fn territory(
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

    pub(super) fn map() -> ClientTerritoryMap {
        [
            territory("Ragni", ("Avicia", "AVO"), [0, 0], [100, 100]),
            territory("Detlas", ("Sequoia", "SEQ"), [200, 0], [300, 100]),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn a_position_is_placed_in_the_territory_containing_it() {
        let map = map();
        assert_eq!(territory_at(&map, 50.0, 50.0).as_deref(), Some("Ragni"));
        assert_eq!(territory_at(&map, 100.0, 100.0).as_deref(), Some("Ragni"));
        assert_eq!(territory_at(&map, 150.0, 50.0), None);
    }

    #[test]
    fn an_ended_war_reads_as_captured_only_once_the_territory_is_ours() {
        let map = map();
        let ended = |territory: &str| Outcome {
            territory: territory.to_string(),
            kind: OutcomeKind::WarEnded,
            at: 1_000,
        };
        assert_eq!(ending(&ended("Detlas"), &map), Ending::Captured);
        assert_eq!(ending(&ended("Ragni"), &map), Ending::Ended);
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
