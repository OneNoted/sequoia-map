//! The war situation the map and HUD both draw, derived from one [`WarControllerState`].
//!
//! The feed carries three loosely joined lists: queue rows, war rows and player heartbeats.
//! [`WarBoard`] joins them once into one [`WarFront`] per territory and one [`RosterMember`]
//! per player, so the map border, the territory plate, the HUD row and the player detail all
//! read the same stage and the same clock.
//!
//! Backend semantics this relies on (`sequoia-backend` `WarStatusService`):
//! - A `QUEUED` row's stamp is the queue expiry, when the party is pulled in. The war starts
//!   [`QUEUE_ENTRY_TO_START_SECS`] later. Its `queued_at`, `queue_seconds` and `queuer` come
//!   from the backend's record of the queue itself.
//! - An `ENTERED` row's stamp is the scheduled start. Its war row already exists, with health
//!   forced to full, so a war row alone does not mean fighting.
//! - Only a `STARTED` row confirms a war. Its stamp is the estimated win, or null.
//! - A `WAR` player heartbeat names the war territory; a `WORLD` heartbeat carries a position.

use std::collections::HashMap;

use crate::{ActiveWar, PlayerClass, QueueStatus, WarControllerState, WarQueueEntry};

/// Seconds between a queue expiring (the party is pulled in) and its war starting, as the
/// backend schedules it.
pub const QUEUE_ENTRY_TO_START_SECS: i64 = 30;

/// A queued front that vanishes at least this long before its queue would have expired was
/// cancelled rather than entered.
const CANCEL_LEAD_SECS: i64 = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct WarBoard {
    /// Fighting first, then entered, then queued; soonest first within each.
    pub fronts: Vec<WarFront>,
    /// Warring members first (by territory), then online members; by name within each.
    pub roster: Vec<RosterMember>,
    /// The map server has no current feed, so the empty board says nothing about the wars.
    pub unavailable: bool,
    /// The feed's own clock when this board was current, Unix seconds.
    pub as_of: i64,
}

/// One territory somewhere between queued and fought over.
#[derive(Debug, Clone, PartialEq)]
pub struct WarFront {
    pub territory: String,
    /// Raw Wynncraft difficulty tier, e.g. `VERY_HIGH`, when the backend classified it.
    pub difficulty: Option<String>,
    pub stage: FrontStage,
    /// While queued: when the queue was made and by whom, each when the backend knows.
    pub queued: Option<QueuedBy>,
}

/// Who made a queue and when, as the backend recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueuedBy {
    pub at: Option<i64>,
    pub queuer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrontStage {
    /// The queue timer is running. `starts_at` is when the war is expected to begin;
    /// `timer` is the queue's own timer, when the backend knows when it started.
    Queued {
        starts_at: Option<i64>,
        timer: Option<QueueTimer>,
    },
    /// The party is inside, waiting for the scheduled start.
    Entered { starts_at: Option<i64> },
    /// Confirmed by a `STARTED` row.
    Fighting {
        started_at: Option<i64>,
        tower: Option<Tower>,
        expected_win_at: Option<i64>,
    },
}

/// A queue's timer as the backend recorded it: from the queue being made to the party being
/// pulled in. The war's start, [`QUEUE_ENTRY_TO_START_SECS`] later, is not part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueTimer {
    pub queued_at: i64,
    pub expires_at: i64,
}

impl QueueTimer {
    /// A timer of `seconds` ending at `expires_at`; anything else is an unknown timer.
    fn new(seconds: Option<i64>, expires_at: Option<i64>) -> Option<Self> {
        let (seconds, expires_at) = (seconds?, expires_at?);
        (seconds > 0).then_some(Self {
            queued_at: expires_at - seconds,
            expires_at,
        })
    }

    /// How long the queue was set for, in seconds.
    pub fn duration(self) -> i64 {
        self.expires_at - self.queued_at
    }

    /// Share of the timer still to run at `now`: 1 when it has just started, 0 once it has
    /// run out.
    pub fn remaining_fraction(self, now: i64) -> f32 {
        ((self.expires_at - now) as f64 / self.duration() as f64).clamp(0.0, 1.0) as f32
    }
}

/// What the map draws on a territory: its stage and the timing its outline may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarMark {
    pub stage: StageKind,
    /// When a queued or entered war is expected to begin; `None` once fighting.
    pub starts_at: Option<i64>,
    pub timer: Option<QueueTimer>,
}

impl WarMark {
    /// The same mark on a clock `secs` behind the feed's.
    pub fn earlier_by(self, secs: i64) -> Self {
        Self {
            starts_at: self.starts_at.map(|at| at - secs),
            timer: self.timer.map(|timer| QueueTimer {
                queued_at: timer.queued_at - secs,
                expires_at: timer.expires_at - secs,
            }),
            ..self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tower {
    /// Remaining fraction, `0.0..=1.0`, from the latest tower sample.
    pub health: f32,
    /// Total effective HP; `None` when the latest sample is stale.
    pub ehp: Option<i64>,
    /// The tower's outgoing damage per second; `None` when the latest sample is stale.
    pub dps: Option<i64>,
}

impl Tower {
    /// Effective HP still standing.
    pub fn remaining_ehp(&self) -> Option<f64> {
        Some(self.ehp? as f64 * f64::from(self.health.clamp(0.0, 1.0)))
    }

    /// Whether the figures come from a fresh tower sample.
    pub fn is_measured(&self) -> bool {
        self.ehp.is_some() || self.dps.is_some()
    }
}

/// What a front's clock reads at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontClock {
    /// Seconds until the war is expected to begin.
    StartsIn(i64),
    /// The expected start has passed but no `STARTED` row has confirmed it.
    Due,
    /// Queued or entered without a usable stamp.
    Unscheduled,
    /// Seconds since a confirmed war began.
    FightingFor(i64),
    /// Confirmed fighting, start time unknown.
    Fighting,
}

impl WarFront {
    pub fn clock(&self, now: i64) -> FrontClock {
        match self.stage {
            FrontStage::Queued { starts_at, .. } | FrontStage::Entered { starts_at } => {
                match starts_at {
                    Some(starts_at) if starts_at > now => FrontClock::StartsIn(starts_at - now),
                    Some(_) => FrontClock::Due,
                    None => FrontClock::Unscheduled,
                }
            }
            FrontStage::Fighting { started_at, .. } => started_at
                .map_or(FrontClock::Fighting, |at| {
                    FrontClock::FightingFor((now - at).max(0))
                }),
        }
    }

    pub fn is_fighting(&self) -> bool {
        matches!(self.stage, FrontStage::Fighting { .. })
    }

    pub fn stage_kind(&self) -> StageKind {
        match self.stage {
            FrontStage::Queued { .. } => StageKind::Queued,
            FrontStage::Entered { .. } => StageKind::Entered,
            FrontStage::Fighting { .. } => StageKind::Fighting,
        }
    }

    /// The queue's own timer, while queued and known.
    pub fn timer(&self) -> Option<QueueTimer> {
        match self.stage {
            FrontStage::Queued { timer, .. } => timer,
            _ => None,
        }
    }

    pub fn mark(&self) -> WarMark {
        let starts_at = match self.stage {
            FrontStage::Queued { starts_at, .. } | FrontStage::Entered { starts_at } => starts_at,
            FrontStage::Fighting { .. } => None,
        };
        WarMark {
            stage: self.stage_kind(),
            starts_at,
            timer: self.timer(),
        }
    }

    /// When the war began or is expected to begin, for ordering.
    fn start_key(&self) -> Option<i64> {
        match self.stage {
            FrontStage::Queued { starts_at, .. } | FrontStage::Entered { starts_at } => starts_at,
            FrontStage::Fighting { started_at, .. } => started_at,
        }
    }
}

/// A front's stage without its timing, for styling and for keys that must not change every
/// second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StageKind {
    Fighting,
    Entered,
    Queued,
}

impl StageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fighting => "Fighting",
            Self::Entered => "Entered",
            Self::Queued => "Queued",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RosterMember {
    pub username: String,
    /// `None` when the class string was not one we know; never guessed.
    pub class: Option<PlayerClass>,
    pub presence: Presence,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Presence {
    /// In a war at this territory.
    Warring { territory: String },
    /// In the world. `territory` is the one containing `position`, when any does.
    Online {
        position: Option<(f64, f64)>,
        territory: Option<String>,
    },
}

impl RosterMember {
    pub fn is_warring(&self) -> bool {
        matches!(self.presence, Presence::Warring { .. })
    }

    /// The territory the member is standing in, when known.
    pub fn territory(&self) -> Option<&str> {
        match &self.presence {
            Presence::Warring { territory } => Some(territory),
            Presence::Online { territory, .. } => territory.as_deref(),
        }
    }

    pub fn position(&self) -> Option<(f64, f64)> {
        match &self.presence {
            Presence::Warring { .. } => None,
            Presence::Online { position, .. } => *position,
        }
    }
}

impl WarBoard {
    /// Joins `state` into fronts and a roster. `locate` names the territory containing a
    /// world position, for members reported by position.
    pub fn new(state: &WarControllerState, locate: impl Fn(f64, f64) -> Option<String>) -> Self {
        Self {
            fronts: build_fronts(state),
            roster: build_roster(state, locate),
            unavailable: state.unavailable,
            as_of: state.timestamp,
        }
    }

    pub fn front(&self, territory: &str) -> Option<&WarFront> {
        self.fronts
            .iter()
            .find(|front| front.territory == territory)
    }

    pub fn member(&self, username: &str) -> Option<&RosterMember> {
        self.roster
            .iter()
            .find(|member| member.username == username)
    }

    /// Members standing inside `territory`, warring or not.
    pub fn members_in<'a>(&'a self, territory: &'a str) -> impl Iterator<Item = &'a RosterMember> {
        self.roster
            .iter()
            .filter(move |member| member.territory() == Some(territory))
    }

    pub fn warring_count(&self) -> usize {
        self.roster
            .iter()
            .filter(|member| member.is_warring())
            .count()
    }

    pub fn online_count(&self) -> usize {
        self.roster.len() - self.warring_count()
    }

    pub fn count(&self, kind: StageKind) -> usize {
        self.fronts
            .iter()
            .filter(|front| front.stage_kind() == kind)
            .count()
    }
}

fn build_fronts(state: &WarControllerState) -> Vec<WarFront> {
    let mut wars: HashMap<&str, &ActiveWar> = HashMap::new();
    for war in &state.wars {
        wars.entry(war.territory.as_str())
            .and_modify(|held| {
                if war.start > held.start {
                    *held = war;
                }
            })
            .or_insert(war);
    }

    let mut fronts: HashMap<&str, WarFront> = HashMap::new();
    for entry in &state.queues {
        let Some(status) = QueueStatus::from_api_status(&entry.status) else {
            continue;
        };
        let front = queue_front(entry, status, wars.get(entry.territory.as_str()).copied());
        match fronts.get(entry.territory.as_str()) {
            Some(held) if !supersedes(&front, held) => {}
            _ => {
                fronts.insert(entry.territory.as_str(), front);
            }
        }
    }
    // A war row with no queue row has no status to confirm it, so it is never read as
    // fighting.
    for (territory, war) in wars {
        fronts.entry(territory).or_insert_with(|| WarFront {
            territory: war.territory.clone(),
            difficulty: war.difficulty.clone(),
            stage: FrontStage::Entered {
                starts_at: Some(war.start),
            },
            queued: None,
        });
    }

    let mut fronts: Vec<WarFront> = fronts.into_values().collect();
    fronts.sort_by(|a, b| {
        a.stage_kind()
            .cmp(&b.stage_kind())
            .then_with(|| a.start_key().is_none().cmp(&b.start_key().is_none()))
            .then_with(|| a.start_key().cmp(&b.start_key()))
            .then_with(|| a.territory.cmp(&b.territory))
    });
    fronts
}

fn queue_front(entry: &WarQueueEntry, status: QueueStatus, war: Option<&ActiveWar>) -> WarFront {
    let stage = match status {
        QueueStatus::Queued => FrontStage::Queued {
            starts_at: entry
                .timestamp
                .map(|expiry| expiry + QUEUE_ENTRY_TO_START_SECS),
            timer: QueueTimer::new(entry.queue_seconds, entry.timestamp),
        },
        QueueStatus::Entered => FrontStage::Entered {
            starts_at: entry.timestamp.or(war.map(|war| war.start)),
        },
        QueueStatus::Started => FrontStage::Fighting {
            started_at: war.map(|war| war.start),
            tower: war.map(|war| Tower {
                health: war.health.clamp(0.0, 1.0),
                ehp: war.ehp,
                dps: war.dps,
            }),
            expected_win_at: entry.timestamp,
        },
    };
    WarFront {
        territory: entry.territory.clone(),
        difficulty: entry
            .difficulty
            .clone()
            .or_else(|| war.and_then(|war| war.difficulty.clone())),
        stage,
        queued: (status == QueueStatus::Queued).then(|| QueuedBy {
            at: entry.queued_at,
            queuer: entry
                .queuer
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        }),
    }
}

/// Of two rows for one territory, the further-along stage wins, then the later stamp.
fn supersedes(candidate: &WarFront, held: &WarFront) -> bool {
    match candidate.stage_kind().cmp(&held.stage_kind()) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => candidate.start_key() > held.start_key(),
    }
}

fn build_roster(
    state: &WarControllerState,
    locate: impl Fn(f64, f64) -> Option<String>,
) -> Vec<RosterMember> {
    let mut seen = std::collections::HashSet::new();
    let mut roster: Vec<RosterMember> = state
        .players
        .iter()
        .filter(|player| seen.insert(player.username.to_lowercase()))
        .map(|player| {
            let position = player.pos.map(|pos| (pos.x, pos.z));
            let presence = match &player.territory {
                Some(territory) => Presence::Warring {
                    territory: territory.clone(),
                },
                None => Presence::Online {
                    position,
                    territory: position.and_then(|(x, z)| locate(x, z)),
                },
            };
            RosterMember {
                username: player.username.clone(),
                class: PlayerClass::from_api_class(&player.class),
                presence,
            }
        })
        .collect();
    roster.sort_by(|a, b| {
        b.is_warring()
            .cmp(&a.is_warring())
            .then_with(|| match (&a.presence, &b.presence) {
                (Presence::Warring { territory: x }, Presence::Warring { territory: y }) => {
                    x.cmp(y)
                }
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
    });
    roster
}

/// How a front left the board, as far as the feed says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// A queued territory vanished well before its queue would have expired.
    Cancelled,
    /// A confirmed war vanished: captured, lost or abandoned. Ownership tells which.
    WarEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub territory: String,
    pub kind: OutcomeKind,
    pub at: i64,
}

/// How an outcome is shown, once territory ownership is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Captured,
    Cancelled,
    Ended,
}

impl Outcome {
    /// `owned` is whether the territory now belongs to the viewer's guild. Ownership comes
    /// from a different feed than the war, so an ended war can become a capture moments later.
    pub fn ending(&self, owned: bool) -> Ending {
        match self.kind {
            OutcomeKind::Cancelled => Ending::Cancelled,
            OutcomeKind::WarEnded if owned => Ending::Captured,
            OutcomeKind::WarEnded => Ending::Ended,
        }
    }
}

/// Fronts that left between `previous` and `next`, judged on the feed's own clock at `next`.
///
/// Nothing is inferred across an unavailable feed: the map server sends one when it loses the
/// backend, and an outage must not read as every war ending at once. Fronts that simply
/// advanced, and queues that reached their entry time, are not outcomes.
pub fn outcomes(previous: &WarBoard, next: &WarBoard) -> Vec<Outcome> {
    let now = next.as_of;
    if previous.unavailable || next.unavailable {
        return Vec::new();
    }
    previous
        .fronts
        .iter()
        .filter(|front| next.front(&front.territory).is_none())
        .filter_map(|front| {
            let kind = match front.stage {
                FrontStage::Fighting { .. } => OutcomeKind::WarEnded,
                FrontStage::Queued {
                    starts_at: Some(starts_at),
                    ..
                } if starts_at - QUEUE_ENTRY_TO_START_SECS - CANCEL_LEAD_SECS > now => {
                    OutcomeKind::Cancelled
                }
                FrontStage::Queued { .. } | FrontStage::Entered { .. } => return None,
            };
            Some(Outcome {
                territory: front.territory.clone(),
                kind,
                at: now,
            })
        })
        .collect()
}

/// `seconds` as `m:ss`, or `h:mm:ss` from an hour.
pub fn format_clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, secs) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WarPlayer, WarPlayerPos};

    const NOW: i64 = 1_000_000;

    fn queue(territory: &str, status: &str, timestamp: Option<i64>) -> WarQueueEntry {
        WarQueueEntry {
            territory: territory.to_string(),
            difficulty: Some("MEDIUM".to_string()),
            status: status.to_string(),
            timestamp,
            queued_at: None,
            queue_seconds: None,
            queuer: None,
        }
    }

    fn timed_queue(territory: &str, queued_at: i64, expires_at: i64) -> WarQueueEntry {
        WarQueueEntry {
            queued_at: Some(queued_at),
            queue_seconds: Some(expires_at - queued_at),
            ..queue(territory, "QUEUED", Some(expires_at))
        }
    }

    fn war(territory: &str, start: i64, health: f32) -> ActiveWar {
        ActiveWar {
            territory: territory.to_string(),
            difficulty: Some("MEDIUM".to_string()),
            health,
            start,
            ehp: Some(400_000),
            dps: Some(900),
        }
    }

    fn player(
        name: &str,
        class: &str,
        territory: Option<&str>,
        pos: Option<(f64, f64)>,
    ) -> WarPlayer {
        WarPlayer {
            username: name.to_string(),
            class: class.to_string(),
            territory: territory.map(str::to_string),
            pos: pos.map(|(x, z)| WarPlayerPos { x, z }),
        }
    }

    fn state(
        queues: Vec<WarQueueEntry>,
        wars: Vec<ActiveWar>,
        players: Vec<WarPlayer>,
    ) -> WarControllerState {
        WarControllerState {
            timestamp: NOW,
            queues,
            wars,
            players,
            unavailable: false,
        }
    }

    fn nowhere(_: f64, _: f64) -> Option<String> {
        None
    }

    fn board(state: &WarControllerState) -> WarBoard {
        WarBoard::new(state, nowhere)
    }

    #[test]
    fn a_queued_front_counts_down_to_the_war_starting_not_to_entry() {
        let board = board(&state(
            vec![queue("Iron Road", "QUEUED", Some(NOW + 90))],
            vec![],
            vec![],
        ));
        assert_eq!(board.fronts[0].clock(NOW), FrontClock::StartsIn(120));
    }

    #[test]
    fn a_queue_keeps_its_original_timer_apart_from_the_countdown_to_the_war() {
        let board = board(&state(
            vec![timed_queue("Detlas", NOW - 120, NOW + 300)],
            vec![],
            vec![],
        ));
        let front = board.front("Detlas").expect("front");

        let timer = front.timer().expect("known timer");
        assert_eq!(timer.duration(), 420);
        assert_eq!(timer.remaining_fraction(NOW), 300.0 / 420.0);
        assert_eq!(timer.remaining_fraction(NOW + 400), 0.0);
        // The countdown still runs to the war's start, 30 s after the queue runs out.
        assert_eq!(front.clock(NOW), FrontClock::StartsIn(330));
        assert_eq!(
            front.mark(),
            WarMark {
                stage: StageKind::Queued,
                starts_at: Some(NOW + 330),
                timer: Some(timer),
            }
        );
    }

    #[test]
    fn an_unknown_or_impossible_queue_start_is_no_timer() {
        let unknown = board(&state(
            vec![queue("Detlas", "QUEUED", Some(NOW + 60))],
            vec![],
            vec![],
        ));
        assert_eq!(unknown.front("Detlas").and_then(WarFront::timer), None);

        let backwards = board(&state(
            vec![timed_queue("Detlas", NOW + 90, NOW + 60)],
            vec![],
            vec![],
        ));
        assert_eq!(backwards.front("Detlas").and_then(WarFront::timer), None);

        let mut no_expiry = timed_queue("Detlas", NOW - 60, NOW);
        no_expiry.timestamp = None;
        let no_expiry = board(&state(vec![no_expiry], vec![], vec![]));
        assert_eq!(no_expiry.front("Detlas").and_then(WarFront::timer), None);
    }

    #[test]
    fn a_queued_front_says_who_queued_it_and_when_even_without_a_real_timer() {
        let mut fallback = queue("Detlas", "QUEUED", Some(NOW + 900));
        fallback.queued_at = Some(NOW);
        fallback.queuer = Some("EpicPuppy613".to_string());
        let mut provisional = timed_queue("Ragni", NOW - 30, NOW + 150);
        provisional.queuer = Some("  ".to_string());
        let board = board(&state(vec![fallback, provisional], vec![], vec![]));

        let detlas = board.front("Detlas").expect("front");
        assert_eq!(detlas.timer(), None);
        assert_eq!(
            detlas.queued,
            Some(QueuedBy {
                at: Some(NOW),
                queuer: Some("EpicPuppy613".to_string()),
            })
        );
        let ragni = board.front("Ragni").expect("front");
        assert_eq!(
            ragni.queued,
            Some(QueuedBy {
                at: Some(NOW - 30),
                queuer: None,
            })
        );
    }

    #[test]
    fn a_requeued_territory_carries_the_new_queue_timer() {
        let first = board(&state(
            vec![timed_queue("Detlas", NOW - 300, NOW + 120)],
            vec![],
            vec![],
        ));
        let requeued = board(&state(
            vec![timed_queue("Detlas", NOW, NOW + 600)],
            vec![],
            vec![],
        ));

        assert_eq!(
            first
                .front("Detlas")
                .and_then(WarFront::timer)
                .map(QueueTimer::duration),
            Some(420)
        );
        let timer = requeued
            .front("Detlas")
            .and_then(WarFront::timer)
            .expect("timer");
        assert_eq!((timer.queued_at, timer.duration()), (NOW, 600));
    }

    #[test]
    fn entered_and_fighting_marks_carry_no_queue_timer() {
        let board = board(&state(
            vec![
                queue("Ragni", "ENTERED", Some(NOW + 20)),
                queue("Detlas", "STARTED", None),
            ],
            vec![war("Ragni", NOW + 20, 1.0), war("Detlas", NOW - 60, 0.5)],
            vec![],
        ));
        assert_eq!(board.front("Ragni").expect("entered").queued, None);
        let entered = board.front("Ragni").expect("entered").mark();
        assert_eq!(
            (entered.stage, entered.starts_at, entered.timer),
            (StageKind::Entered, Some(NOW + 20), None)
        );
        let fighting = board.front("Detlas").expect("fighting").mark();
        assert_eq!(
            (fighting.stage, fighting.starts_at, fighting.timer),
            (StageKind::Fighting, None, None)
        );
    }

    #[test]
    fn an_entered_front_with_a_full_health_war_row_is_not_fighting() {
        // The backend creates the war row on entry, health forced to 1.0, 30 s before it starts.
        let board = board(&state(
            vec![queue("Iron Road", "ENTERED", Some(NOW + 20))],
            vec![war("Iron Road", NOW + 20, 1.0)],
            vec![],
        ));
        let front = &board.fronts[0];
        assert!(!front.is_fighting());
        assert_eq!(front.clock(NOW), FrontClock::StartsIn(20));
    }

    #[test]
    fn a_passed_start_without_confirmation_is_due_not_fighting() {
        let board = board(&state(
            vec![queue("Iron Road", "ENTERED", Some(NOW + 20))],
            vec![war("Iron Road", NOW + 20, 1.0)],
            vec![],
        ));
        assert_eq!(board.fronts[0].clock(NOW + 25), FrontClock::Due);
        assert_eq!(board.fronts[0].clock(NOW + 20), FrontClock::Due);
    }

    #[test]
    fn missing_timing_is_unscheduled_rather_than_zero() {
        let board = board(&state(
            vec![queue("A", "QUEUED", None), queue("B", "ENTERED", None)],
            vec![],
            vec![],
        ));
        for front in &board.fronts {
            assert_eq!(front.clock(NOW), FrontClock::Unscheduled);
        }
    }

    #[test]
    fn a_war_row_without_any_queue_row_is_never_read_as_fighting() {
        let board = board(&state(vec![], vec![war("A", NOW - 60, 0.4)], vec![]));
        assert!(!board.fronts[0].is_fighting());
        assert_eq!(board.fronts[0].clock(NOW), FrontClock::Due);
    }

    #[test]
    fn a_started_front_reads_elapsed_time_and_the_tower() {
        let board = board(&state(
            vec![queue("A", "STARTED", Some(NOW + 95))],
            vec![war("A", NOW - 47, 0.62)],
            vec![],
        ));
        let front = &board.fronts[0];
        assert_eq!(front.clock(NOW), FrontClock::FightingFor(47));
        let FrontStage::Fighting {
            tower: Some(tower),
            expected_win_at,
            ..
        } = front.stage
        else {
            panic!("started front is fighting with a tower");
        };
        assert_eq!(expected_win_at, Some(NOW + 95));
        assert!((tower.remaining_ehp().expect("measured") - 248_000.0).abs() < 1.0);
    }

    #[test]
    fn a_started_front_with_a_stale_tower_has_no_figures() {
        let mut stale = war("A", NOW - 10, 0.3);
        stale.ehp = None;
        stale.dps = None;
        let board = board(&state(
            vec![queue("A", "STARTED", None)],
            vec![stale],
            vec![],
        ));
        let FrontStage::Fighting {
            tower: Some(tower), ..
        } = board.fronts[0].stage
        else {
            panic!("started front has a tower reading");
        };
        assert!(!tower.is_measured());
        assert_eq!(tower.remaining_ehp(), None);
    }

    #[test]
    fn fronts_order_fighting_entered_queued_soonest_first() {
        let board = board(&state(
            vec![
                queue("Late", "QUEUED", Some(NOW + 400)),
                queue("Unstamped", "QUEUED", None),
                queue("Soon", "QUEUED", Some(NOW + 60)),
                queue("Inside", "ENTERED", Some(NOW + 10)),
                queue("War", "STARTED", None),
            ],
            vec![war("War", NOW - 5, 0.9), war("Inside", NOW + 10, 1.0)],
            vec![],
        ));
        let order: Vec<&str> = board.fronts.iter().map(|f| f.territory.as_str()).collect();
        assert_eq!(order, ["War", "Inside", "Soon", "Late", "Unstamped"]);
    }

    #[test]
    fn a_repeated_territory_keeps_its_furthest_stage() {
        let board = board(&state(
            vec![
                queue("A", "QUEUED", Some(NOW + 60)),
                queue("A", "STARTED", None),
            ],
            vec![war("A", NOW - 5, 0.9)],
            vec![],
        ));
        assert_eq!(board.fronts.len(), 1);
        assert!(board.fronts[0].is_fighting());
    }

    #[test]
    fn unknown_statuses_are_dropped() {
        let board = board(&state(vec![queue("A", "LEFT", Some(NOW))], vec![], vec![]));
        assert!(board.fronts.is_empty());
    }

    #[test]
    fn roster_separates_warring_from_online_and_locates_positions() {
        let locate = |x: f64, _z: f64| (x > 0.0).then(|| "Ragni".to_string());
        let board = WarBoard::new(
            &state(
                vec![],
                vec![],
                vec![
                    player("zed", "MAGE", None, Some((10.0, 5.0))),
                    player("amy", "WARRIOR", Some("Azure Frontier"), None),
                    player("bob", "ARCHER", None, Some((-10.0, 5.0))),
                    player("cat", "BARD", None, None),
                ],
            ),
            locate,
        );
        let names: Vec<&str> = board.roster.iter().map(|m| m.username.as_str()).collect();
        assert_eq!(names, ["amy", "bob", "cat", "zed"]);
        assert_eq!(board.warring_count(), 1);
        assert_eq!(board.online_count(), 3);
        assert_eq!(
            board.member("zed").and_then(RosterMember::territory),
            Some("Ragni")
        );
        assert_eq!(board.member("bob").and_then(RosterMember::territory), None);
        assert_eq!(
            board.member("bob").and_then(RosterMember::position),
            Some((-10.0, 5.0))
        );
        // An unknown class stays unknown.
        assert_eq!(board.member("cat").and_then(|m| m.class), None);
        assert_eq!(
            board.member("amy").and_then(|m| m.class),
            Some(PlayerClass::Warrior)
        );
    }

    #[test]
    fn members_in_a_queued_territory_include_those_standing_there_by_position() {
        let locate = |_x: f64, _z: f64| Some("Lizardman Camp".to_string());
        let board = WarBoard::new(
            &state(
                vec![queue("Lizardman Camp", "QUEUED", Some(NOW + 60))],
                vec![],
                vec![
                    player("Yearnm", "ASSASSIN", None, Some((1.0, 1.0))),
                    player("Other", "MAGE", Some("Azure Frontier"), None),
                ],
            ),
            locate,
        );
        let inside: Vec<&str> = board
            .members_in("Lizardman Camp")
            .map(|m| m.username.as_str())
            .collect();
        assert_eq!(inside, ["Yearnm"]);
    }

    #[test]
    fn a_repeated_player_is_listed_once() {
        let board = board(&state(
            vec![],
            vec![],
            vec![
                player("amy", "MAGE", None, None),
                player("Amy", "MAGE", None, None),
            ],
        ));
        assert_eq!(board.roster.len(), 1);
    }

    #[test]
    fn a_queue_removed_early_is_a_cancellation() {
        let previous = board(&state(
            vec![queue("A", "QUEUED", Some(NOW + 300))],
            vec![],
            vec![],
        ));
        let next = state(vec![], vec![], vec![player("x", "MAGE", None, None)]);
        assert_eq!(
            outcomes(&previous, &board(&next)),
            vec![Outcome {
                territory: "A".to_string(),
                kind: OutcomeKind::Cancelled,
                at: NOW
            }]
        );
    }

    #[test]
    fn a_queue_leaving_at_its_expiry_is_not_a_cancellation() {
        let previous = board(&state(
            vec![queue("A", "QUEUED", Some(NOW + 2))],
            vec![],
            vec![],
        ));
        let next = state(vec![queue("B", "QUEUED", Some(NOW + 60))], vec![], vec![]);
        assert!(outcomes(&previous, &board(&next)).is_empty());
    }

    #[test]
    fn a_fighting_front_that_leaves_has_ended() {
        let previous = board(&state(
            vec![queue("A", "STARTED", None)],
            vec![war("A", NOW - 30, 0.1)],
            vec![],
        ));
        let next = state(vec![queue("B", "QUEUED", Some(NOW + 60))], vec![], vec![]);
        assert_eq!(
            outcomes(&previous, &board(&next))[0].kind,
            OutcomeKind::WarEnded
        );
    }

    #[test]
    fn an_advancing_front_is_not_an_outcome() {
        let previous = board(&state(
            vec![queue("A", "QUEUED", Some(NOW + 300))],
            vec![],
            vec![],
        ));
        let next = state(
            vec![queue("A", "ENTERED", Some(NOW + 30))],
            vec![war("A", NOW + 30, 1.0)],
            vec![],
        );
        assert!(outcomes(&previous, &board(&next)).is_empty());
    }

    #[test]
    fn an_unavailable_feed_infers_no_outcomes() {
        // What the map server broadcasts when it loses the backend.
        let previous = board(&state(
            vec![
                queue("A", "STARTED", None),
                queue("B", "QUEUED", Some(NOW + 300)),
            ],
            vec![war("A", NOW - 30, 0.4)],
            vec![],
        ));
        let lost = board(&WarControllerState::unavailable(NOW));
        assert!(lost.unavailable);
        assert!(outcomes(&previous, &lost).is_empty());
    }

    #[test]
    fn the_last_war_ending_on_an_empty_feed_is_still_an_outcome() {
        let previous = board(&state(
            vec![queue("A", "STARTED", None)],
            vec![war("A", NOW - 30, 0.1)],
            vec![],
        ));
        let quiet = board(&state(vec![], vec![], vec![]));
        assert_eq!(outcomes(&previous, &quiet)[0].kind, OutcomeKind::WarEnded);
    }

    #[test]
    fn an_ended_war_reads_as_captured_only_once_the_territory_is_ours() {
        let ended = Outcome {
            territory: "A".to_string(),
            kind: OutcomeKind::WarEnded,
            at: NOW,
        };
        assert_eq!(ended.ending(true), Ending::Captured);
        assert_eq!(ended.ending(false), Ending::Ended);
        let cancelled = Outcome {
            kind: OutcomeKind::Cancelled,
            ..ended
        };
        assert_eq!(cancelled.ending(true), Ending::Cancelled);
    }

    #[test]
    fn clocks_format_minutes_and_hours() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(125), "2:05");
        assert_eq!(format_clock(3_725), "1:02:05");
        assert_eq!(format_clock(-4), "0:00");
    }
}
