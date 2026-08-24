use std::collections::HashSet;

use serde::de::{self, Deserializer, Unexpected};
use serde::{Deserialize, Serialize};

/// Live war controller feed from the Sequoia backend: territories queued for war,
/// wars currently in progress, and the players participating in them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarControllerState {
    /// Unix seconds at which the backend generated this payload.
    #[serde(deserialize_with = "deserialize_lenient_timestamp")]
    pub timestamp: i64,
    #[serde(default)]
    pub queues: Vec<WarQueueEntry>,
    #[serde(default)]
    pub wars: Vec<ActiveWar>,
    #[serde(default)]
    pub players: Vec<WarPlayer>,
}

impl WarControllerState {
    /// Territory names with a war in progress.
    ///
    /// Presence in [`Self::wars`] is the only "at war" signal the feed carries: there is no
    /// `ended` flag, the backend simply drops the entry once the war finishes, and
    /// [`ActiveWar::health`] reaching `0.0` does not mean finished. Queued territories are
    /// deliberately excluded — they live in [`Self::queues`] until their war actually starts.
    pub fn territories_at_war(&self) -> HashSet<String> {
        self.wars.iter().map(|war| war.territory.clone()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarQueueEntry {
    pub territory: String,
    /// Wynncraft difficulty tier, e.g. `VERY_HIGH`. Parse with [`TreasuryLevel::from_api_tier`].
    pub difficulty: String,
    /// Queue stage, e.g. `QUEUED`. Parse with [`QueueStatus::from_api_status`].
    pub status: String,
    /// Unix seconds - but of *what* depends on [`Self::status`].
    ///
    /// For `QUEUED` and `ENTERED` it is the instant the entry entered that status. For
    /// `STARTED` the backend overwrites it with the instant the war is expected to be won,
    /// which is how the ETA actually reaches us. Read it through [`Self::eta_secs`] rather
    /// than subtracting by hand.
    #[serde(deserialize_with = "deserialize_lenient_timestamp")]
    pub timestamp: i64,
    /// Seconds until the war is expected to be won, counted from [`WarControllerState::timestamp`].
    ///
    /// Speculative: the backend has never sent this field, and ships the ETA in
    /// [`Self::timestamp`] instead. Parsed anyway so it takes precedence if that changes.
    #[serde(default, deserialize_with = "deserialize_optional_eta_secs")]
    pub eta: Option<i64>,
}

impl WarQueueEntry {
    /// Seconds until the war is expected to be won, relative to
    /// [`WarControllerState::timestamp`], or `None` when this entry carries no ETA.
    ///
    /// A `STARTED` entry's [`Self::timestamp`] *is* the expected win instant, so the time
    /// remaining is that instant minus the snapshot the payload was built at. The explicit
    /// [`Self::eta`] field wins when present, since a backend that starts sending it means it
    /// deliberately. `QUEUED` and `ENTERED` entries have no ETA at all - nothing in the feed
    /// predicts when a war that has not started yet will be won.
    ///
    /// Floored at zero: a war can overrun its predicted win time, and the browser clock the
    /// caller counts down against can sit either side of the feed's timestamps.
    pub fn eta_secs(&self, feed_timestamp: i64) -> Option<i64> {
        if let Some(eta) = self.eta.filter(|seconds| *seconds >= 0) {
            return Some(eta);
        }
        if QueueStatus::from_api_status(&self.status)? != QueueStatus::Started {
            return None;
        }
        Some((self.timestamp - feed_timestamp).max(0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveWar {
    pub territory: String,
    pub difficulty: String,
    /// Remaining tower health as a fraction in `0.0..=1.0`.
    pub health: f32,
    /// Unix seconds at which the war started.
    #[serde(deserialize_with = "deserialize_lenient_timestamp")]
    pub start: i64,
    #[serde(default)]
    pub ehp: Option<i64>,
    #[serde(default)]
    pub dps: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarPlayer {
    pub username: String,
    /// Wynncraft class, e.g. `WARRIOR`. Parse with [`PlayerClass::from_api_class`].
    pub class: String,
    /// Territory the player is currently in, if any.
    #[serde(default)]
    pub territory: Option<String>,
    /// World position, present when the player is not inside a tracked territory.
    #[serde(default)]
    pub pos: Option<WarPlayerPos>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarPlayerPos {
    #[serde(deserialize_with = "deserialize_lenient_f64")]
    pub x: f64,
    #[serde(deserialize_with = "deserialize_lenient_f64")]
    pub z: f64,
}

/// Stage of a territory in the war queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueStatus {
    Queued,
    Entered,
    Started,
}

impl QueueStatus {
    pub fn from_api_status(raw: &str) -> Option<Self> {
        let normalized = raw.trim().replace([' ', '-'], "_").to_ascii_uppercase();
        match normalized.as_str() {
            "QUEUED" => Some(Self::Queued),
            "ENTERED" => Some(Self::Entered),
            "STARTED" => Some(Self::Started),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Entered => "Entered",
            Self::Started => "Started",
        }
    }
}

/// Wynncraft player class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerClass {
    Archer,
    Assassin,
    Mage,
    Shaman,
    Warrior,
}

impl PlayerClass {
    pub fn from_api_class(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_uppercase();
        match normalized.as_str() {
            "ARCHER" | "HUNTER" => Some(Self::Archer),
            "ASSASSIN" | "NINJA" => Some(Self::Assassin),
            "MAGE" | "DARK_WIZARD" | "DARKWIZARD" => Some(Self::Mage),
            "SHAMAN" | "SKYSEER" => Some(Self::Shaman),
            "WARRIOR" | "KNIGHT" => Some(Self::Warrior),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Archer => "Archer",
            Self::Assassin => "Assassin",
            Self::Mage => "Mage",
            Self::Shaman => "Shaman",
            Self::Warrior => "Warrior",
        }
    }
}

/// The backend is inconsistent about coordinate encoding, sending some components as
/// JSON numbers and others as numeric strings, so accept both.
fn deserialize_lenient_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    match LenientF64::deserialize(deserializer)? {
        LenientF64::Number(value) => Ok(value),
        LenientF64::Text(raw) => raw.trim().parse::<f64>().map_err(|_| {
            de::Error::invalid_value(Unexpected::Str(&raw), &"a number or numeric string")
        }),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LenientF64 {
    Number(f64),
    Text(String),
}

/// Timestamps arrive as Unix seconds in the war controller payload, but every other
/// Sequoia backend endpoint serializes instants as RFC3339 strings, so accept both and
/// normalize to Unix seconds.
fn deserialize_lenient_timestamp<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    match LenientTimestamp::deserialize(deserializer)? {
        LenientTimestamp::Epoch(value) => Ok(value),
        LenientTimestamp::Text(raw) => parse_timestamp(&raw).ok_or_else(|| {
            de::Error::invalid_value(
                Unexpected::Str(&raw),
                &"Unix seconds or an RFC3339 timestamp",
            )
        }),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LenientTimestamp {
    Epoch(i64),
    Text(String),
}

/// The ETA is not in the feed yet, so its eventual encoding is unknown. Accept a number or
/// a numeric string and read anything else as "no ETA" rather than failing: erroring here
/// would reject the whole payload and take the live war feed down over a cosmetic column.
fn deserialize_optional_eta_secs<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            Some(serde_json::Value::Number(value)) => value
                .as_i64()
                .or_else(|| value.as_f64().map(|seconds| seconds.trunc() as i64)),
            Some(serde_json::Value::String(raw)) => raw.trim().parse::<i64>().ok(),
            _ => None,
        },
    )
}

fn parse_timestamp(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if let Ok(seconds) = trimmed.parse::<i64>() {
        return Some(seconds);
    }
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|value| value.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treasury::TreasuryLevel;

    const SAMPLE: &str = r#"{
        "timestamp": 1787517420,
        "queues": [
            {"territory": "Entrance to Olux", "difficulty": "VERY_HIGH", "status": "STARTED", "timestamp": 1787517443},
            {"territory": "Overtaken Outpost", "difficulty": "VERY_LOW", "status": "ENTERED", "timestamp": 1787517461},
            {"territory": "Mangled Lake", "difficulty": "VERY_LOW", "status": "QUEUED", "timestamp": 1787517489}
        ],
        "wars": [
            {"territory": "Entrance to Olux", "difficulty": "VERY_HIGH", "health": 0.8731, "start": 1787517417, "ehp": 24135275, "dps": 32143},
            {"territory": "Overtaken Outpost", "difficulty": "VERY_LOW", "health": 1.0, "start": 1787517455, "ehp": null, "dps": null}
        ],
        "players": [
            {"username": "EpicPuppy613", "class": "WARRIOR", "territory": "Entrance to Olux", "pos": null},
            {"username": "pat_crafter07", "class": "WARRIOR", "territory": "Entrance to Olux", "pos": null},
            {"username": "shisouhan", "class": "SHAMAN", "territory": "Entrance to Olux", "pos": null},
            {"username": "starfaiien", "class": "MAGE", "territory": "Overtaken Outpost", "pos": null},
            {"username": "Yearnm", "class": "MAGE", "territory": null, "pos": {"x": -1517, "z": "-5130"}}
        ]
    }"#;

    #[test]
    fn parses_sample_payload() {
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");

        assert_eq!(state.timestamp, 1787517420);
        assert_eq!(state.queues.len(), 3);
        assert_eq!(state.wars.len(), 2);
        assert_eq!(state.players.len(), 5);

        let queued = &state.queues[2];
        assert_eq!(queued.territory, "Mangled Lake");
        assert_eq!(
            QueueStatus::from_api_status(&queued.status),
            Some(QueueStatus::Queued)
        );
        assert_eq!(
            TreasuryLevel::from_api_tier(&queued.difficulty),
            Some(TreasuryLevel::VeryLow)
        );
        assert_eq!(queued.timestamp, 1787517489);

        let war = &state.wars[0];
        assert_eq!(war.territory, "Entrance to Olux");
        assert!((war.health - 0.8731).abs() < 1e-6);
        assert_eq!(war.start, 1787517417);
        assert_eq!(war.ehp, Some(24135275));
        assert_eq!(war.dps, Some(32143));
    }

    #[test]
    fn territories_at_war_covers_active_wars_only() {
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");
        let at_war = state.territories_at_war();

        assert!(at_war.contains("Entrance to Olux"));
        assert!(at_war.contains("Overtaken Outpost"));
        // Queued but not yet fighting, so it must not be flagged as at war.
        assert!(!at_war.contains("Mangled Lake"));
        assert_eq!(at_war.len(), 2);
    }

    #[test]
    fn territories_at_war_is_empty_without_wars() {
        let state: WarControllerState =
            serde_json::from_str(r#"{"timestamp": 42}"#).expect("minimal payload parses");
        assert!(state.territories_at_war().is_empty());
    }

    #[test]
    fn null_ehp_and_dps_are_none() {
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");
        let war = &state.wars[1];
        assert_eq!(war.ehp, None);
        assert_eq!(war.dps, None);
    }

    #[test]
    fn pos_accepts_number_and_numeric_string() {
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");

        let roaming = &state.players[4];
        assert_eq!(roaming.username, "Yearnm");
        assert_eq!(roaming.territory, None);
        assert_eq!(
            PlayerClass::from_api_class(&roaming.class),
            Some(PlayerClass::Mage)
        );

        let pos = roaming.pos.expect("roaming player has a position");
        assert_eq!(pos.x, -1517.0);
        assert_eq!(pos.z, -5130.0);

        let in_territory = &state.players[0];
        assert_eq!(in_territory.territory.as_deref(), Some("Entrance to Olux"));
        assert_eq!(in_territory.pos, None);
    }

    #[test]
    fn eta_is_absent_when_the_backend_omits_it() {
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");
        assert!(state.queues.iter().all(|entry| entry.eta.is_none()));
    }

    #[test]
    fn eta_accepts_numbers_and_numeric_strings() {
        let raw = r#"{
            "timestamp": 1,
            "queues": [
                {"territory": "A", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": 90},
                {"territory": "B", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": "45"},
                {"territory": "C", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": 12.7}
            ]
        }"#;

        let state: WarControllerState = serde_json::from_str(raw).expect("eta payload parses");
        assert_eq!(state.queues[0].eta, Some(90));
        assert_eq!(state.queues[1].eta, Some(45));
        assert_eq!(state.queues[2].eta, Some(12));
    }

    #[test]
    fn unreadable_eta_reads_as_none_instead_of_failing_the_payload() {
        // A future backend could encode the ETA some other way; the war feed must keep
        // flowing regardless, with only the ETA column going blank.
        let raw = r#"{
            "timestamp": 1,
            "queues": [
                {"territory": "A", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": null},
                {"territory": "B", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": "2026-08-24T12:37:00Z"},
                {"territory": "C", "difficulty": "LOW", "status": "QUEUED", "timestamp": 1, "eta": {"seconds": 30}}
            ]
        }"#;

        let state: WarControllerState = serde_json::from_str(raw).expect("odd eta payload parses");
        assert_eq!(state.queues.len(), 3);
        assert!(state.queues.iter().all(|entry| entry.eta.is_none()));
    }

    /// A queue entry carrying only the fields `eta_secs` reads.
    fn queue_entry(status: &str, timestamp: i64, eta: Option<i64>) -> WarQueueEntry {
        WarQueueEntry {
            territory: "Entrance to Olux".to_string(),
            difficulty: "VERY_HIGH".to_string(),
            status: status.to_string(),
            timestamp,
            eta,
        }
    }

    #[test]
    fn started_entry_derives_its_eta_from_the_timestamp() {
        // The sampled payload: the STARTED entry's timestamp sits 23s past the snapshot,
        // because the backend overwrites it with the expected win instant.
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");
        let started = &state.queues[0];
        assert_eq!(started.status, "STARTED");
        assert_eq!(started.eta_secs(state.timestamp), Some(23));
    }

    #[test]
    fn queued_and_entered_entries_have_no_eta() {
        // Nothing in the feed predicts when a war that has not started will be won, and their
        // timestamps mean something else entirely - the instant they entered that status.
        let state: WarControllerState = serde_json::from_str(SAMPLE).expect("sample parses");
        assert_eq!(state.queues[1].status, "ENTERED");
        assert_eq!(state.queues[1].eta_secs(state.timestamp), None);
        assert_eq!(state.queues[2].status, "QUEUED");
        assert_eq!(state.queues[2].eta_secs(state.timestamp), None);
    }

    #[test]
    fn an_explicit_eta_field_wins_over_the_timestamp() {
        // A backend that starts sending `eta` alongside the timestamp means it.
        let entry = queue_entry("STARTED", 1_000_000, Some(42));
        assert_eq!(entry.eta_secs(999_000), Some(42));
        // And it carries stages that have no derived ETA of their own.
        assert_eq!(queue_entry("QUEUED", 0, Some(90)).eta_secs(0), Some(90));
    }

    #[test]
    fn a_negative_eta_field_falls_through_to_the_timestamp() {
        let entry = queue_entry("STARTED", 1_180, Some(-5));
        assert_eq!(entry.eta_secs(1_000), Some(180));
    }

    #[test]
    fn a_past_started_timestamp_floors_at_zero() {
        // A war can overrun its predicted win time; the ETA holds at zero rather than
        // counting back up.
        assert_eq!(queue_entry("STARTED", 900, None).eta_secs(1_000), Some(0));
    }

    #[test]
    fn an_unknown_status_has_no_eta() {
        assert_eq!(queue_entry("LEFT", 1_180, None).eta_secs(1_000), None);
    }

    #[test]
    fn timestamps_accept_rfc3339_strings() {
        // Every other Sequoia backend endpoint serializes instants this way, so the
        // war controller feed may well ship RFC3339 rather than the sampled integers.
        let raw = r#"{
            "timestamp": "2026-08-24T12:37:00Z",
            "queues": [{"territory": "Mangled Lake", "difficulty": "VERY_LOW", "status": "QUEUED", "timestamp": "2026-08-24T12:37:09Z"}],
            "wars": [{"territory": "Mangled Lake", "difficulty": "VERY_LOW", "health": 1.0, "start": "2026-08-24T12:36:57+00:00"}]
        }"#;

        let state: WarControllerState = serde_json::from_str(raw).expect("rfc3339 payload parses");
        assert_eq!(state.timestamp, 1_787_575_020);
        assert_eq!(state.queues[0].timestamp, 1_787_575_029);
        assert_eq!(state.wars[0].start, 1_787_575_017);
    }

    #[test]
    fn timestamps_accept_numeric_strings() {
        let raw = r#"{"timestamp": "1787517420"}"#;
        let state: WarControllerState = serde_json::from_str(raw).expect("numeric string parses");
        assert_eq!(state.timestamp, 1787517420);
    }

    #[test]
    fn rejects_unparseable_timestamp() {
        let raw = r#"{"timestamp": "not a time"}"#;
        assert!(serde_json::from_str::<WarControllerState>(raw).is_err());
    }

    #[test]
    fn rejects_non_numeric_position_string() {
        let raw = r#"{"timestamp": 1, "players": [{"username": "x", "class": "MAGE", "pos": {"x": "abc", "z": 0}}]}"#;
        assert!(serde_json::from_str::<WarControllerState>(raw).is_err());
    }

    #[test]
    fn missing_collections_default_to_empty() {
        let state: WarControllerState =
            serde_json::from_str(r#"{"timestamp": 42}"#).expect("minimal payload parses");
        assert_eq!(state.timestamp, 42);
        assert!(state.queues.is_empty());
        assert!(state.wars.is_empty());
        assert!(state.players.is_empty());
    }

    #[test]
    fn unknown_enum_labels_parse_to_none() {
        assert_eq!(QueueStatus::from_api_status("LEFT"), None);
        assert_eq!(PlayerClass::from_api_class("BARD"), None);
        assert_eq!(
            QueueStatus::from_api_status(" entered "),
            Some(QueueStatus::Entered)
        );
    }
}
