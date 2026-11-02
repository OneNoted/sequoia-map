use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::SeasonScalarSample;
use crate::territory_state::{TerritoryState, TerritoryStateField};

/// Snapshot of season rating data for one guild at a given sample time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryGuildSrEntry {
    pub guild_uuid: String,
    pub guild_name: String,
    pub guild_prefix: String,
    pub sampled_at: String,
    pub season_id: i32,
    pub season_rating: i64,
    pub season_rank: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sr_gain_5m: Option<i64>,
}

/// Collection of season rating snapshots captured at a single timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySrSnapshot {
    pub sampled_at: String,
    pub entries: Vec<HistoryGuildSrEntry>,
}

/// Ownership record for a single territory at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipRecord {
    pub guild_uuid: String,
    pub guild_name: String,
    pub guild_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_color: Option<(u8, u8, u8)>,
    pub acquired_at: String,
}

/// Reconstructed state of all territory ownership at a specific timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub timestamp: String,
    pub ownership: HashMap<String, OwnershipRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_scalar: Option<SeasonScalarSample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_leaderboard: Option<Vec<HistoryGuildSrEntry>>,
    /// Official-API state of owned territories as last observed, absent where none was
    /// recorded by then. Its owner can differ from `ownership` while reporters lead the API.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub states: HashMap<String, HistoryTerritoryState>,
}

/// A territory's official-API state at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryTerritoryState {
    /// When the API supplied the latest change to this state.
    pub observed_at: String,
    pub state: TerritoryState,
}

/// Official-API state changes recorded at one stream position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryStateEvent {
    pub stream_seq: u64,
    pub timestamp: String,
    pub observed_at: String,
    pub changes: Vec<HistoryStateChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryStateChange {
    pub territory: String,
    pub fields: Vec<TerritoryStateField>,
}

/// A single territory change event from the history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    #[serde(default)]
    pub stream_seq: u64,
    /// Order within a stream position; a canonical batch shares one position.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ordinal: u16,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired_at: Option<String>,
    pub territory: String,
    pub guild_uuid: String,
    pub guild_name: String,
    pub guild_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_color: Option<(u8, u8, u8)>,
    pub prev_guild_name: Option<String>,
    pub prev_guild_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_guild_color: Option<(u8, u8, u8)>,
}

/// Paginated list of history events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvents {
    pub events: Vec<HistoryEvent>,
    /// State changes in the same window and order, at their own stream positions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<HistoryStateEvent>,
    pub has_more: bool,
}

/// Season rating snapshots over a time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySrSamples {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<HistorySrSnapshot>,
}

/// Time bounds and event count for the history timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryBounds {
    pub earliest: Option<String>,
    pub latest: Option<String>,
    pub event_count: i64,
    pub latest_seq: Option<u64>,
}

/// Heat map data source used for territory takeover aggregation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryHeatSource {
    Season,
    AllTime,
}

/// Time window metadata for a specific season id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHeatSeasonWindow {
    pub season_id: i32,
    pub start: String,
    pub end: String,
    pub is_current: bool,
}

/// Metadata required by client-side heat-map controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHeatMeta {
    pub latest_season_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seasons: Vec<HistoryHeatSeasonWindow>,
    pub all_time_earliest: Option<String>,
    pub retention_days: i64,
    pub season_fallback_days: i64,
}

/// Per-territory takeover count entry for the selected heat window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHeatEntry {
    pub territory: String,
    pub take_count: u64,
}

/// Aggregated territory heat-map response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHeat {
    pub source: HistoryHeatSource,
    pub season_id: Option<i32>,
    pub from: String,
    pub to: String,
    pub fallback_applied: bool,
    pub max_take_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<HistoryHeatEntry>,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}
