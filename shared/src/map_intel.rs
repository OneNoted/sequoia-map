use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapIntelSummary {
    pub generated_at: String,
    pub source: String,
    pub raids: MapActivityCollectionSummary,
    pub camps: MapActivityCollectionSummary,
    pub world_events: WorldEventCollectionSummary,
    pub gathering_nodes: GatheringNodeCollectionSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapIntelOverlay {
    pub generated_at: String,
    pub source: String,
    pub raids: Vec<MapActivityMarker>,
    pub camps: Vec<MapActivityMarker>,
    pub world_events: Vec<WorldEventMarker>,
    pub gathering_nodes: Vec<GatheringNodeMarker>,
    pub gathering_resources: Vec<NamedCount>,
    pub gathering_node_types: Vec<NamedCount>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedCount {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MapPoint {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapActivityCollectionSummary {
    pub count: usize,
    pub min_level: Option<i32>,
    pub max_level: Option<i32>,
    pub difficulties: Vec<NamedCount>,
    pub lengths: Vec<NamedCount>,
    pub entries: Vec<MapActivitySummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapActivitySummary {
    pub name: String,
    pub internal_name: String,
    pub kind: String,
    pub difficulty: Option<String>,
    pub level: Option<i32>,
    pub length: Option<String>,
    pub location: Option<MapPoint>,
    pub requirement_count: usize,
    pub rewards: MapRewardSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapActivityMarker {
    pub name: String,
    pub internal_name: String,
    pub kind: String,
    pub difficulty: Option<String>,
    pub level: Option<i32>,
    pub length: Option<String>,
    pub location: MapPoint,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRewardSummary {
    pub total: usize,
    pub always: usize,
    pub mythic: usize,
    pub fabled: usize,
    pub legendary: usize,
    pub rare: usize,
    pub unique: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldEventCollectionSummary {
    pub count: usize,
    pub scheduled_count: usize,
    pub next_schedule: Option<String>,
    pub min_level: Option<i32>,
    pub max_level: Option<i32>,
    pub difficulties: Vec<NamedCount>,
    pub lengths: Vec<NamedCount>,
    pub scheduled: Vec<WorldEventSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldEventSummary {
    pub name: String,
    pub internal_name: String,
    pub difficulty: Option<String>,
    pub level: Option<i32>,
    pub length: Option<String>,
    pub schedule: Option<String>,
    pub location_count: usize,
    pub first_location: Option<MapPoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldEventMarker {
    pub name: String,
    pub internal_name: String,
    pub difficulty: Option<String>,
    pub level: Option<i32>,
    pub length: Option<String>,
    pub schedule: Option<String>,
    pub locations: Vec<MapPoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GatheringNodeCollectionSummary {
    pub count: usize,
    pub min_level: Option<i32>,
    pub max_level: Option<i32>,
    pub resources: Vec<NamedCount>,
    pub node_types: Vec<NamedCount>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GatheringNodeMarker {
    pub location: MapPoint,
    pub node_type: String,
    pub resource: String,
    pub level: Option<i32>,
    pub angle: Option<f64>,
}
