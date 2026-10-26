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
    /// Set by the map server, never the backend, on the empty state it sends while it has no
    /// current feed: the backend is unreachable, or has not answered since the server started.
    /// Tells a client "unknown" apart from "no war activity".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
}

impl WarControllerState {
    /// The map server's stand-in for a feed it does not have, stamped `timestamp` on its own
    /// clock.
    pub fn unavailable(timestamp: i64) -> Self {
        Self {
            timestamp,
            queues: Vec::new(),
            wars: Vec::new(),
            players: Vec::new(),
            unavailable: true,
        }
    }

    /// Whether this payload may replace `current`, comparing [`Self::timestamp`].
    ///
    /// The feed reaches a client three ways - the REST seed, the SSE seed, and every live SSE
    /// frame - and they can land out of order: a seed fetch can be overtaken by the stream, and
    /// the REST endpoint answers with an empty `timestamp: 0` state while the server holds
    /// nothing cached. Without this check either one can resurrect a war that has already
    /// ended. The server applies the same rule to what it emits.
    ///
    /// Equal timestamps supersede: the poller only broadcasts on a real change, so a repeat is
    /// a re-seed of the same state rather than a step backwards.
    ///
    /// The map server's clears ([`Self::unavailable`]) are the cases the timestamp does not
    /// decide. A clear is stamped on the map server's clock while every real frame is stamped
    /// by the backend, so the two cannot be ordered by stamp: whichever clock ran ahead would
    /// strand the client, on "unavailable" after the backend recovers or on wars that are no
    /// longer known after it goes away. Real data, quiet or not, therefore replaces a held
    /// clear, and a live clear replaces whatever is held. The REST endpoint's clear is stamped
    /// zero and, coming from a request that may have been overtaken by the stream, only fills
    /// an empty slot.
    pub fn supersedes(&self, current: Option<&Self>) -> bool {
        match current {
            None => true,
            Some(current) if current.unavailable && !self.unavailable => true,
            Some(_) if self.unavailable && self.timestamp > 0 => true,
            Some(current) => self.timestamp >= current.timestamp,
        }
    }

    /// Whether two payloads describe the same war situation, ignoring [`Self::timestamp`].
    ///
    /// The backend restamps that field on every response, so a plain `==` between a cached
    /// payload and a freshly fetched one is *never* true in production - which would have the
    /// poller rebroadcast the whole feed to every subscriber on every tick, and would quietly
    /// falsify the "a repeat means a re-seed" rule that [`Self::supersedes`] rests on.
    pub fn same_content(&self, other: &Self) -> bool {
        self.queues == other.queues
            && self.wars == other.wars
            && self.players == other.players
            && self.unavailable == other.unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarQueueEntry {
    pub territory: String,
    /// Wynncraft difficulty tier, e.g. `VERY_HIGH`, or `None` when the backend has not
    /// classified the territory. Parse with [`TreasuryLevel::from_api_tier`].
    #[serde(default)]
    pub difficulty: Option<String>,
    /// Queue stage, e.g. `QUEUED`. Parse with [`QueueStatus::from_api_status`].
    pub status: String,
    /// Unix seconds at which this entry next changes: the queue expiry for `QUEUED` (the
    /// party is pulled in, and the war starts 30 s later), the scheduled start for `ENTERED`,
    /// and the estimated win for `STARTED`. [`crate::war_board`] turns it into a clock.
    ///
    /// `None` when the backend sends no stamp, as it does for a `STARTED` war whose tower
    /// sample is stale.
    #[serde(default, deserialize_with = "deserialize_optional_lenient_timestamp")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveWar {
    pub territory: String,
    /// Wynncraft difficulty tier, or `None` when the backend has not classified the territory.
    #[serde(default)]
    pub difficulty: Option<String>,
    /// Remaining tower health as a fraction in `0.0..=1.0`.
    pub health: f32,
    /// Unix seconds at which the war started.
    #[serde(deserialize_with = "deserialize_lenient_timestamp")]
    pub start: i64,
    /// The tower's *total* effective HP. Multiply by [`Self::health`] for what is still
    /// standing. `None` on a war the backend has not measured yet.
    #[serde(default)]
    pub ehp: Option<i64>,
    /// The tower's *outgoing* damage per second - what it deals to the attackers, not what it
    /// takes from them. Never a time-to-kill input; the ETA comes from the queue entry instead.
    /// `None` on a war the backend has not measured yet.
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

/// The nullable sibling of [`deserialize_lenient_timestamp`], for the one stamp the backend
/// may omit: a queue entry's. `null` and a missing key both read as "no stamp", while a value
/// that *is* present still has to be a real instant - accepting garbage there would silently
/// erase an ETA rather than surface a backend change.
fn deserialize_optional_lenient_timestamp<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<LenientTimestamp>::deserialize(deserializer)? {
        None => Ok(None),
        Some(LenientTimestamp::Epoch(value)) => Ok(Some(value)),
        Some(LenientTimestamp::Text(raw)) => parse_timestamp(&raw).map(Some).ok_or_else(|| {
            de::Error::invalid_value(
                Unexpected::Str(&raw),
                &"Unix seconds or an RFC3339 timestamp",
            )
        }),
    }
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
            queued
                .difficulty
                .as_deref()
                .and_then(TreasuryLevel::from_api_tier),
            Some(TreasuryLevel::VeryLow)
        );
        assert_eq!(queued.timestamp, Some(1787517489));

        let war = &state.wars[0];
        assert_eq!(war.territory, "Entrance to Olux");
        assert!((war.health - 0.8731).abs() < 1e-6);
        assert_eq!(war.start, 1787517417);
        assert_eq!(war.ehp, Some(24135275));
        assert_eq!(war.dps, Some(32143));
    }

    fn state_at(timestamp: i64) -> WarControllerState {
        WarControllerState {
            timestamp,
            queues: Vec::new(),
            wars: Vec::new(),
            players: Vec::new(),
            unavailable: false,
        }
    }

    #[test]
    fn an_unavailable_feed_is_marked_on_the_wire_and_a_backend_payload_is_not() {
        let cleared = serde_json::to_value(WarControllerState::unavailable(5)).expect("serializes");
        assert_eq!(cleared["unavailable"], true);

        let backend: WarControllerState =
            serde_json::from_str(r#"{"timestamp": 1}"#).expect("parses");
        assert!(!backend.unavailable);
        let echoed = serde_json::to_value(&backend).expect("serializes");
        assert!(echoed.get("unavailable").is_none());
    }

    #[test]
    fn real_data_supersedes_an_unavailable_feed() {
        assert!(war_state_at(900).supersedes(Some(&WarControllerState::unavailable(1_000))));
    }

    #[test]
    fn anything_supersedes_a_client_holding_nothing() {
        assert!(state_at(0).supersedes(None));
    }

    #[test]
    fn payloads_differing_only_in_their_stamp_have_the_same_content() {
        // Exactly what a live backend sends every five seconds while nothing is happening.
        assert!(state_at(1_000).same_content(&state_at(2_000)));

        let mut changed = state_at(1_000);
        changed.queues.push(WarQueueEntry {
            territory: "Entrance to Olux".to_string(),
            difficulty: Some("VERY_HIGH".to_string()),
            status: "STARTED".to_string(),
            timestamp: Some(1_787_517_443),
        });
        assert!(!changed.same_content(&state_at(1_000)));
    }

    #[test]
    fn a_newer_or_equal_payload_supersedes() {
        let held = state_at(1_000);
        assert!(state_at(1_001).supersedes(Some(&held)));
        // A re-seed of the same snapshot, not a step backwards.
        assert!(state_at(1_000).supersedes(Some(&held)));
    }

    #[test]
    fn an_older_payload_does_not_supersede() {
        // The REST endpoint's empty fallback must not resurrect a war the stream already ended.
        let held = state_at(1_000);
        assert!(!state_at(999).supersedes(Some(&held)));
        assert!(!state_at(0).supersedes(Some(&held)));
    }

    /// A payload with real war activity, for the rules that turn on emptiness.
    fn war_state_at(timestamp: i64) -> WarControllerState {
        let mut state = state_at(timestamp);
        state.wars.push(ActiveWar {
            territory: "Entrance to Olux".to_string(),
            difficulty: Some("VERY_HIGH".to_string()),
            health: 0.5,
            start: timestamp,
            ehp: None,
            dps: None,
        });
        state
    }

    #[test]
    fn real_data_supersedes_a_held_clear_whatever_the_clocks_say() {
        // The clear is stamped on the map server's clock, the recovered frame on the
        // backend's. A backend running behind us must not leave the client on "unavailable",
        // whether it recovers with wars or with nothing going on.
        let cleared = WarControllerState::unavailable(1_000);
        assert!(war_state_at(900).supersedes(Some(&cleared)));
        assert!(state_at(900).supersedes(Some(&cleared)));
    }

    #[test]
    fn a_live_clear_replaces_held_wars_whatever_the_clocks_say() {
        // A backend running ahead of us must not keep its last wars on screen once the map
        // server has dropped them.
        let held = war_state_at(1_000);
        assert!(WarControllerState::unavailable(999).supersedes(Some(&held)));
    }

    #[test]
    fn the_rest_fallback_clear_never_replaces_what_is_held() {
        // Stamped zero: the request may have been overtaken by the stream.
        let held = war_state_at(1_000);
        assert!(!WarControllerState::unavailable(0).supersedes(Some(&held)));
        assert!(WarControllerState::unavailable(0).supersedes(None));
    }

    #[test]
    fn an_older_quiet_backend_frame_does_not_replace_live_wars() {
        // Only the map server's clears skip the stamp comparison: a quiet backend frame is
        // still just a frame, and an older one must not end a war the client is watching.
        let held = war_state_at(1_000);
        assert!(!state_at(999).supersedes(Some(&held)));
        assert!(state_at(1_001).supersedes(Some(&held)));
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
    fn null_difficulty_and_timestamp_parse_rather_than_failing_the_payload() {
        // The backend classifies a territory and stamps its queue entry when it can, and
        // sends null when it cannot. Rejecting either would take the whole live feed down.
        let raw = r#"{
            "timestamp": 1787517420,
            "queues": [
                {"territory": "A", "difficulty": null, "status": "STARTED", "timestamp": null}
            ],
            "wars": [
                {"territory": "A", "difficulty": null, "health": 0.5, "start": 1787517417}
            ]
        }"#;

        let state: WarControllerState = serde_json::from_str(raw).expect("null payload parses");
        assert_eq!(state.queues[0].difficulty, None);
        assert_eq!(state.queues[0].timestamp, None);
        assert_eq!(state.wars[0].difficulty, None);
    }

    #[test]
    fn omitted_difficulty_and_timestamp_read_the_same_as_null() {
        let raw = r#"{
            "timestamp": 1787517420,
            "queues": [{"territory": "A", "status": "QUEUED"}],
            "wars": [{"territory": "A", "health": 0.5, "start": 1787517417}]
        }"#;

        let state: WarControllerState = serde_json::from_str(raw).expect("sparse payload parses");
        assert_eq!(state.queues[0].difficulty, None);
        assert_eq!(state.queues[0].timestamp, None);
        assert_eq!(state.wars[0].difficulty, None);
    }

    #[test]
    fn a_nullable_timestamp_still_rejects_a_value_that_is_not_an_instant() {
        // Nullable is not the same as tolerant: a stamp that is present but unreadable would
        // silently erase an ETA, and that is a backend change worth surfacing.
        let raw = r#"{
            "timestamp": 1787517420,
            "queues": [{"territory": "A", "status": "STARTED", "timestamp": "soon"}]
        }"#;
        assert!(serde_json::from_str::<WarControllerState>(raw).is_err());
    }

    #[test]
    fn a_nullable_timestamp_still_accepts_an_rfc3339_string() {
        let raw = r#"{
            "timestamp": 1787575020,
            "queues": [
                {"territory": "A", "status": "STARTED", "timestamp": "2026-08-24T12:37:09Z"}
            ]
        }"#;
        let state: WarControllerState = serde_json::from_str(raw).expect("rfc3339 payload parses");
        assert_eq!(state.queues[0].timestamp, Some(1_787_575_029));
    }

    #[test]
    fn a_null_bearing_payload_survives_a_round_trip() {
        // The SSE layer re-serializes the state it parsed, so a null that becomes something
        // else on the way out would reach the client as a different payload than arrived.
        let raw = r#"{
            "timestamp": 1787517420,
            "queues": [
                {"territory": "A", "difficulty": null, "status": "STARTED", "timestamp": null}
            ],
            "wars": [
                {"territory": "A", "difficulty": null, "health": 0.5, "start": 1787517417}
            ]
        }"#;
        let state: WarControllerState = serde_json::from_str(raw).expect("null payload parses");
        let round_tripped: WarControllerState =
            serde_json::from_str(&serde_json::to_string(&state).expect("state serializes"))
                .expect("re-serialized payload parses");
        assert_eq!(state, round_tripped);
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
        assert_eq!(state.queues[0].timestamp, Some(1_787_575_029));
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
