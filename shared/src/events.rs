use serde::{Deserialize, Serialize};

use crate::ingest::TerritoryRuntimeChange;
use crate::territory::{GuildRef, Region, Resources, TerritoryMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TerritoryEvent {
    Snapshot {
        #[serde(default)]
        seq: u64,
        territories: TerritoryMap,
        timestamp: String,
    },
    Update {
        #[serde(default)]
        seq: u64,
        changes: Vec<TerritoryChange>,
        timestamp: String,
    },
    RuntimeUpdate {
        #[serde(default)]
        seq: u64,
        updates: Vec<TerritoryRuntimeChange>,
        timestamp: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveState {
    pub seq: u64,
    pub timestamp: String,
    pub territories: TerritoryMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerritoryChange {
    pub territory: String,
    pub guild: GuildRef,
    pub previous_guild: Option<GuildRef>,
    pub acquired: String,
    pub location: Region,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub connections: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::ingest::TerritoryRuntimeData>,
}
