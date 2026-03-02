use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::territory::{GuildRef, Region, Resources};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityClass {
    #[default]
    Public,
    GuildOptIn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataProvenance {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub visibility: VisibilityClass,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub reporter_count: u16,
    #[serde(default)]
    pub observed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_season_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_captured_territories: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_sr_per_hour: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_observed_at: Option<String>,
}

impl Default for DataProvenance {
    fn default() -> Self {
        Self {
            source: "unknown".to_string(),
            visibility: VisibilityClass::Public,
            confidence: 0.0,
            reporter_count: 0,
            observed_at: String::new(),
            menu_season_id: None,
            menu_captured_territories: None,
            menu_sr_per_hour: None,
            menu_observed_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TerritoryRuntimeData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headquarters: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production_rates: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_capacity: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defense_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contested: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_war: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_scrapes: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<DataProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TerritoryRuntimeChange {
    pub territory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<TerritoryRuntimeData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarTowerState {
    #[serde(default)]
    pub health: i64,
    #[serde(default)]
    pub defense: f64,
    #[serde(default)]
    pub damage_low: i64,
    #[serde(default)]
    pub damage_high: i64,
    #[serde(default)]
    pub attack_speed: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarEventKind {
    Queued,
    Started,
    Ended,
    Captured,
    TowerState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarEvent {
    #[serde(default)]
    pub id: String,
    pub kind: WarEventKind,
    pub territory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild: Option<GuildRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tower_state: Option<WarTowerState>,
    #[serde(default)]
    pub observed_at: String,
    #[serde(default)]
    pub provenance: DataProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalTerritoryUpdate {
    pub territory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild: Option<GuildRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Region>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<TerritoryRuntimeData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CanonicalTerritoryBatch {
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub updates: Vec<CanonicalTerritoryUpdate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalWarReport {
    pub event: WarEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CanonicalWarBatch {
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub events: Vec<CanonicalWarReport>,
}
