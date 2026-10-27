//! Recent war activity, as the backend's journal reports it: Sequoia's captures and losses,
//! cancelled queues and attacks that ended without a confirmed capture, newest first.
//!
//! Every measurement is optional and stays absent when nobody measured it. Times are Unix
//! seconds. A page is bounded by the backend's read window; `next_before` continues it.

use serde::{Deserialize, Serialize};

/// The window members can read back through, as the backend keeps it.
pub const ACTIVITY_READ_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarActivityPage {
    pub server_time: i64,
    #[serde(default)]
    pub entries: Vec<ActivityEntry>,
    /// Pass back as `before` for the next older page; `None` at the end of the range.
    #[serde(default)]
    pub next_before: Option<String>,
    /// The oldest time the backend keeps entries for.
    pub kept_since: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub territory: String,
    pub outcome: ActivityOutcome,
    pub occurred_at: i64,
    /// False when only the time it was first seen is known.
    pub occurred_at_exact: bool,
    #[serde(default)]
    pub difficulty: Option<String>,
    #[serde(default)]
    pub queue: Option<ActivityQueue>,
    #[serde(default)]
    pub war: Option<ActivityWar>,
    #[serde(default)]
    pub owner_change: Option<OwnerChange>,
    /// A capture several observed attacks could have produced; none of them is attached.
    #[serde(default)]
    pub attempt_ambiguous: bool,
    /// This entry's place in the order, for continuing a page after it.
    pub cursor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActivityOutcome {
    /// A Sequoia gain, confirmed by the territory changing hands.
    Captured,
    /// A Sequoia territory taken by another guild.
    Lost,
    /// A queue its owner cancelled.
    Cancelled,
    /// An attack that stopped without an ownership change to confirm a capture.
    Ended,
}

impl ActivityOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Captured => "Captured",
            Self::Lost => "Lost",
            Self::Cancelled => "Cancelled",
            Self::Ended => "Ended",
        }
    }

    /// Captures and losses: what the list shows unless asked for everything.
    pub fn is_decisive(self) -> bool {
        matches!(self, Self::Captured | Self::Lost)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityQueue {
    #[serde(default)]
    pub queuer: Option<String>,
    #[serde(default)]
    pub queued_at: Option<i64>,
    /// The original queue timer; `None` when it was a guess.
    #[serde(default)]
    pub seconds: Option<i64>,
}

/// What was observed of an attack. Each time is only what it says it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityWar {
    /// The start a queue scheduled; `None` when no queue scheduled it.
    #[serde(default)]
    pub starts_at: Option<i64>,
    #[serde(default)]
    pub start_source: Option<StartSource>,
    /// The first report from a member in it.
    #[serde(default)]
    pub first_reported_at: Option<i64>,
    /// The last report from a member in it; not when the fighting ended.
    #[serde(default)]
    pub last_reported_at: Option<i64>,
    /// When a member reported it complete; `None` otherwise.
    #[serde(default)]
    pub ended_at: Option<i64>,
    /// The reports stopped and resumed: it was not observed throughout.
    #[serde(default)]
    pub interrupted: bool,
    /// Its reports followed an earlier attack's on the territory after a gap too long to tell
    /// whether it is the same attack.
    #[serde(default)]
    pub after_gap: bool,
    #[serde(default)]
    pub tower: Option<ActivityTower>,
    /// Members who reported being in the war, in the order they joined.
    #[serde(default)]
    pub participants: Vec<String>,
}

/// Where a scheduled start came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StartSource {
    /// An observed queue timer.
    Queue,
    /// The fifteen-minute guess for a queue whose timer nobody saw.
    Estimate,
}

impl ActivityWar {
    /// How long the war was fought: only between a start an observed queue timer scheduled and
    /// a reported end. Anything else would be a guess.
    pub fn fought_secs(&self) -> Option<i64> {
        if self.start_source != Some(StartSource::Queue) {
            return None;
        }
        let (start, end) = (self.starts_at?, self.ended_at?);
        (end > start).then_some(end - start)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityTower {
    #[serde(default)]
    pub ehp: Option<i64>,
    #[serde(default)]
    pub dps: Option<i64>,
    #[serde(default)]
    pub first_health: Option<f32>,
    #[serde(default)]
    pub observed_at: Option<i64>,
    /// True only when the first sample was taken at full health by the start.
    pub from_start: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnerChange {
    pub from: ActivityGuild,
    pub to: ActivityGuild,
    pub changed_at: i64,
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityGuild {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The backend's own example of a confirmed capture and an unmeasured ending.
    const PAGE: &str = r#"{
        "server_time": 1787520000,
        "entries": [
            {"id": 12, "territory": "Detlas", "outcome": "CAPTURED", "occurred_at": 1787519885,
             "occurred_at_exact": true, "difficulty": "HIGH",
             "queue": {"queuer": "EpicPuppy613", "queued_at": 1787519400, "seconds": 420},
             "war": {"starts_at": 1787519850, "start_source": "QUEUE", "first_reported_at": 1787519820,
                     "last_reported_at": 1787519879, "ended_at": 1787519880, "interrupted": false,
                     "tower": {"ehp": 400000, "dps": 3750, "first_health": 1.0, "observed_at": 1787519851,
                               "from_start": true},
                     "participants": ["EpicPuppy613", "pat_crafter07"]},
             "owner_change": {"from": {"name": "Avicia", "prefix": "AVO"},
                              "to": {"name": "Sequoia", "prefix": "SEQ"},
                              "changed_at": 1787519885, "exact": true},
             "attempt_ambiguous": false,
             "cursor": "1787519885000000.12"},
            {"id": 11, "territory": "Ragni", "outcome": "ENDED", "occurred_at": 1787519000,
             "occurred_at_exact": false, "difficulty": null, "queue": null,
             "war": {"starts_at": null, "start_source": null, "first_reported_at": 1787518900,
                     "last_reported_at": 1787519000, "ended_at": null, "interrupted": true,
                     "after_gap": true, "tower": null, "participants": []},
             "owner_change": null, "attempt_ambiguous": false, "cursor": "1787519000000000.11"}
        ],
        "next_before": "1787519000000000.11",
        "kept_since": 1786915200
    }"#;

    #[test]
    fn a_page_parses_with_its_snapshots_and_absences() {
        let page: WarActivityPage = serde_json::from_str(PAGE).expect("page parses");
        let capture = &page.entries[0];
        assert_eq!(capture.outcome, ActivityOutcome::Captured);
        assert!(capture.outcome.is_decisive());
        let war = capture.war.as_ref().expect("war");
        assert_eq!(war.fought_secs(), Some(30));
        assert_eq!(war.start_source, Some(StartSource::Queue));
        assert_eq!(
            war.tower.as_ref().and_then(|tower| tower.ehp),
            Some(400_000)
        );

        let quiet = &page.entries[1];
        assert_eq!(quiet.outcome, ActivityOutcome::Ended);
        let war = quiet.war.as_ref().expect("war");
        assert!(war.interrupted && war.after_gap);
        assert!(!capture.war.as_ref().expect("war").after_gap);
        assert_eq!((war.starts_at, war.ended_at), (None, None));
        assert_eq!(war.fought_secs(), None);
        assert_eq!(page.next_before.as_deref(), Some("1787519000000000.11"));
    }

    #[test]
    fn only_an_observed_queued_start_and_a_reported_end_give_a_fight_time() {
        let war = |source, ended_at| ActivityWar {
            starts_at: Some(130),
            start_source: source,
            first_reported_at: Some(100),
            last_reported_at: Some(200),
            ended_at,
            interrupted: false,
            after_gap: false,
            tower: None,
            participants: Vec::new(),
        };
        assert_eq!(
            war(Some(StartSource::Queue), Some(190)).fought_secs(),
            Some(60)
        );
        assert_eq!(
            war(Some(StartSource::Estimate), Some(190)).fought_secs(),
            None
        );
        assert_eq!(war(None, Some(190)).fought_secs(), None);
        assert_eq!(war(Some(StartSource::Queue), None).fought_secs(), None);
        assert_eq!(war(Some(StartSource::Queue), Some(120)).fought_secs(), None);
    }
}
