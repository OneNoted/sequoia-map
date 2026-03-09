use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::territory::{GuildRef, Region, Resources, TerritoryMap};
use crate::tower::count_guild_connections;

pub const CLAIM_DOCUMENT_VERSION_V1: u8 = 1;
pub const MAX_CLAIM_DOCUMENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimOwner {
    Neutral,
    Guild { guild: GuildRef },
}

impl ClaimOwner {
    pub fn neutral() -> Self {
        Self::Neutral
    }

    pub fn from_guild(guild: GuildRef) -> Self {
        Self::Guild { guild }
    }

    pub fn as_guild(&self) -> Option<&GuildRef> {
        match self {
            ClaimOwner::Neutral => None,
            ClaimOwner::Guild { guild } => Some(guild),
        }
    }

    pub fn identity_key(&self) -> Option<String> {
        let guild = self.as_guild()?;
        if !guild.uuid.trim().is_empty() {
            return Some(format!("uuid:{}", guild.uuid.trim()));
        }
        Some(format!("name:{}", guild.name.trim().to_ascii_lowercase()))
    }

    pub fn display_name(&self) -> &str {
        match self {
            ClaimOwner::Neutral => "Neutral",
            ClaimOwner::Guild { guild } => guild.name.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaimsBootstrapGeometry {
    #[serde(default)]
    pub territories: HashMap<String, ClaimsTerritoryGeometry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimsTerritoryGeometry {
    pub location: Region,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub connections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimDocumentBase {
    Blank,
    FrozenLiveSnapshot {
        captured_at: String,
        seq: u64,
        owners: HashMap<String, ClaimOwner>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimMacro {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub territories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaimViewState {
    pub offset_x: f64,
    pub offset_y: f64,
    pub scale: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_owner: Option<ClaimOwner>,
}

impl Default for ClaimViewState {
    fn default() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.3,
            active_owner: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimDocumentV1 {
    pub version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub base: ClaimDocumentBase,
    #[serde(default)]
    pub overrides: HashMap<String, ClaimOwner>,
    #[serde(default)]
    pub macros: Vec<ClaimMacro>,
    #[serde(default)]
    pub view: ClaimViewState,
}

impl Default for ClaimDocumentV1 {
    fn default() -> Self {
        Self {
            version: CLAIM_DOCUMENT_VERSION_V1,
            title: None,
            base: ClaimDocumentBase::Blank,
            overrides: HashMap::new(),
            macros: Vec::new(),
            view: ClaimViewState::default(),
        }
    }
}

impl ClaimDocumentV1 {
    pub fn blank() -> Self {
        Self::default()
    }

    pub fn frozen_live(
        title: Option<String>,
        seq: u64,
        owners: HashMap<String, ClaimOwner>,
    ) -> Self {
        Self {
            version: CLAIM_DOCUMENT_VERSION_V1,
            title,
            base: ClaimDocumentBase::FrozenLiveSnapshot {
                captured_at: Utc::now().to_rfc3339(),
                seq,
                owners,
            },
            overrides: HashMap::new(),
            macros: Vec::new(),
            view: ClaimViewState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimValidationError {
    UnsupportedVersion(u8),
    DocumentTooLarge(usize),
    UnknownTerritory(String),
    DuplicateMacroId(String),
    EmptyMacroName(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaimResourceCounts {
    pub territories: u32,
    pub emerald: u32,
    pub ore: u32,
    pub crops: u32,
    pub fish: u32,
    pub wood: u32,
    pub rainbow: u32,
    pub any_double: u32,
    pub double_emerald: u32,
    pub double_ore: u32,
    pub double_crops: u32,
    pub double_fish: u32,
    pub double_wood: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimHubMetrics {
    pub territory: String,
    pub guild_connections: u32,
    pub total_connections: u32,
    pub externals: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimGuildMetrics {
    pub owner: ClaimOwner,
    pub territory_count: u32,
    pub changed_territory_count: u32,
    pub resources: ClaimResourceCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_by_connections: Option<ClaimHubMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_by_externals: Option<ClaimHubMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaimMetrics {
    pub total_territories: u32,
    pub neutral_territories: u32,
    #[serde(default)]
    pub guilds: Vec<ClaimGuildMetrics>,
}

fn base_owner_for(document: &ClaimDocumentV1, territory: &str) -> ClaimOwner {
    match &document.base {
        ClaimDocumentBase::Blank => ClaimOwner::Neutral,
        ClaimDocumentBase::FrozenLiveSnapshot { owners, .. } => owners
            .get(territory)
            .cloned()
            .unwrap_or(ClaimOwner::Neutral),
    }
}

pub fn claim_document_size(document: &ClaimDocumentV1) -> Result<usize, serde_json::Error> {
    serde_json::to_vec(document).map(|bytes| bytes.len())
}

pub fn validate_claim_document<'a>(
    document: &ClaimDocumentV1,
    territories: impl IntoIterator<Item = &'a str>,
) -> Result<(), ClaimValidationError> {
    if document.version != CLAIM_DOCUMENT_VERSION_V1 {
        return Err(ClaimValidationError::UnsupportedVersion(document.version));
    }

    let document_size = claim_document_size(document).unwrap_or(MAX_CLAIM_DOCUMENT_BYTES + 1);
    if document_size > MAX_CLAIM_DOCUMENT_BYTES {
        return Err(ClaimValidationError::DocumentTooLarge(document_size));
    }

    let territory_names: HashSet<&str> = territories.into_iter().collect();

    let ensure_known = |territory: &str| {
        if territory_names.contains(territory) {
            Ok(())
        } else {
            Err(ClaimValidationError::UnknownTerritory(
                territory.to_string(),
            ))
        }
    };

    if let ClaimDocumentBase::FrozenLiveSnapshot { owners, .. } = &document.base {
        for territory in owners.keys() {
            ensure_known(territory)?;
        }
    }

    for territory in document.overrides.keys() {
        ensure_known(territory)?;
    }

    let mut macro_ids = HashSet::new();
    for macro_entry in &document.macros {
        if macro_entry.name.trim().is_empty() {
            return Err(ClaimValidationError::EmptyMacroName(macro_entry.id.clone()));
        }
        if !macro_ids.insert(macro_entry.id.as_str()) {
            return Err(ClaimValidationError::DuplicateMacroId(
                macro_entry.id.clone(),
            ));
        }
        for territory in &macro_entry.territories {
            ensure_known(territory)?;
        }
    }

    Ok(())
}

pub fn materialize_claim_owners(
    document: &ClaimDocumentV1,
    territories: &TerritoryMap,
) -> HashMap<String, ClaimOwner> {
    let mut owners = HashMap::with_capacity(territories.len());
    for territory in territories.keys() {
        owners.insert(territory.clone(), base_owner_for(document, territory));
    }
    for (territory, owner) in &document.overrides {
        if owners.contains_key(territory) {
            owners.insert(territory.clone(), owner.clone());
        }
    }
    owners
}

pub fn compact_claim_overrides(
    document: &ClaimDocumentV1,
    territories: &TerritoryMap,
) -> HashMap<String, ClaimOwner> {
    let mut overrides = HashMap::new();
    for (territory, owner) in &document.overrides {
        if !territories.contains_key(territory) {
            continue;
        }
        let base_owner = base_owner_for(document, territory);
        if *owner != base_owner {
            overrides.insert(territory.clone(), owner.clone());
        }
    }
    overrides
}

pub fn compute_claim_metrics(
    document: &ClaimDocumentV1,
    territories: &TerritoryMap,
) -> ClaimMetrics {
    let effective = materialize_claim_owners(document, territories);
    let owner_keys: HashMap<String, String> = effective
        .iter()
        .filter_map(|(territory, owner)| owner.identity_key().map(|key| (territory.clone(), key)))
        .collect();
    let mut guild_metrics: HashMap<String, ClaimGuildMetrics> = HashMap::new();
    let mut neutral_territories = 0u32;

    for (territory_name, territory) in territories {
        let owner = effective
            .get(territory_name)
            .cloned()
            .unwrap_or(ClaimOwner::Neutral);
        let base_owner = base_owner_for(document, territory_name);
        if matches!(owner, ClaimOwner::Neutral) {
            neutral_territories = neutral_territories.saturating_add(1);
            continue;
        }

        let key = owner.identity_key().unwrap_or_default();
        let entry = guild_metrics
            .entry(key.clone())
            .or_insert_with(|| ClaimGuildMetrics {
                owner: owner.clone(),
                territory_count: 0,
                changed_territory_count: 0,
                resources: ClaimResourceCounts::default(),
                top_by_connections: None,
                top_by_externals: None,
            });

        entry.territory_count = entry.territory_count.saturating_add(1);
        entry.resources.territories = entry.resources.territories.saturating_add(1);
        if owner != base_owner {
            entry.changed_territory_count = entry.changed_territory_count.saturating_add(1);
        }

        let resources = &territory.resources;
        if resources.emeralds > 0 {
            entry.resources.emerald = entry.resources.emerald.saturating_add(1);
        }
        if resources.ore > 0 {
            entry.resources.ore = entry.resources.ore.saturating_add(1);
        }
        if resources.crops > 0 {
            entry.resources.crops = entry.resources.crops.saturating_add(1);
        }
        if resources.fish > 0 {
            entry.resources.fish = entry.resources.fish.saturating_add(1);
        }
        if resources.wood > 0 {
            entry.resources.wood = entry.resources.wood.saturating_add(1);
        }
        if resources.has_all() {
            entry.resources.rainbow = entry.resources.rainbow.saturating_add(1);
        }
        let has_any_double = resources.has_double_emeralds()
            || resources.has_double_ore()
            || resources.has_double_crops()
            || resources.has_double_fish()
            || resources.has_double_wood();
        if has_any_double {
            entry.resources.any_double = entry.resources.any_double.saturating_add(1);
        }
        if resources.has_double_emeralds() {
            entry.resources.double_emerald = entry.resources.double_emerald.saturating_add(1);
        }
        if resources.has_double_ore() {
            entry.resources.double_ore = entry.resources.double_ore.saturating_add(1);
        }
        if resources.has_double_crops() {
            entry.resources.double_crops = entry.resources.double_crops.saturating_add(1);
        }
        if resources.has_double_fish() {
            entry.resources.double_fish = entry.resources.double_fish.saturating_add(1);
        }
        if resources.has_double_wood() {
            entry.resources.double_wood = entry.resources.double_wood.saturating_add(1);
        }

        let hub = build_hub_metrics(territory_name, territory, territories, &owner_keys, &key);
        update_hub_metric(&mut entry.top_by_connections, hub.clone(), true);
        update_hub_metric(&mut entry.top_by_externals, hub, false);
    }

    let mut guilds: Vec<ClaimGuildMetrics> = guild_metrics.into_values().collect();
    guilds.sort_by(|a, b| {
        let a_externals = a
            .top_by_externals
            .as_ref()
            .map(|hub| hub.externals)
            .unwrap_or_default();
        let b_externals = b
            .top_by_externals
            .as_ref()
            .map(|hub| hub.externals)
            .unwrap_or_default();
        b.territory_count
            .cmp(&a.territory_count)
            .then_with(|| b_externals.cmp(&a_externals))
            .then_with(|| a.owner.display_name().cmp(b.owner.display_name()))
    });

    ClaimMetrics {
        total_territories: territories.len() as u32,
        neutral_territories,
        guilds,
    }
}

fn build_hub_metrics(
    territory_name: &str,
    territory: &crate::territory::Territory,
    territories: &TerritoryMap,
    owner_keys: &HashMap<String, String>,
    owner_key: &str,
) -> ClaimHubMetrics {
    let (guild_connections, total_connections, externals) = count_guild_connections(
        territory_name,
        territory.connections.as_slice(),
        owner_key,
        |neighbor| {
            let territory = territories.get(neighbor)?;
            let key = owner_keys.get(neighbor)?;
            Some((key.as_str(), territory.connections.as_slice()))
        },
    );

    ClaimHubMetrics {
        territory: territory_name.to_string(),
        guild_connections,
        total_connections,
        externals,
    }
}

fn update_hub_metric(
    current: &mut Option<ClaimHubMetrics>,
    candidate: ClaimHubMetrics,
    by_connections: bool,
) {
    let ordering = match current {
        Some(existing) => compare_hub_metrics(existing, &candidate, by_connections),
        None => Ordering::Greater,
    };
    if ordering == Ordering::Greater {
        *current = Some(candidate);
    }
}

fn compare_hub_metrics(
    existing: &ClaimHubMetrics,
    candidate: &ClaimHubMetrics,
    by_connections: bool,
) -> Ordering {
    let (existing_primary, candidate_primary) = if by_connections {
        (existing.guild_connections, candidate.guild_connections)
    } else {
        (existing.externals, candidate.externals)
    };

    candidate_primary
        .cmp(&existing_primary)
        .then_with(|| candidate.externals.cmp(&existing.externals))
        .then_with(|| candidate.guild_connections.cmp(&existing.guild_connections))
        .then_with(|| existing.territory.cmp(&candidate.territory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::territory::{Region, Resources, Territory};

    fn guild(uuid: &str, name: &str, prefix: &str) -> ClaimOwner {
        ClaimOwner::from_guild(GuildRef {
            uuid: uuid.to_string(),
            name: name.to_string(),
            prefix: prefix.to_string(),
            color: None,
        })
    }

    fn make_territory(connections: &[&str], resources: Resources) -> Territory {
        Territory {
            guild: GuildRef {
                uuid: "live".to_string(),
                name: "Live".to_string(),
                prefix: "LIV".to_string(),
                color: None,
            },
            acquired: Utc::now(),
            location: Region {
                start: [0, 0],
                end: [10, 10],
            },
            resources,
            connections: connections
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            runtime: None,
        }
    }

    fn sample_map() -> TerritoryMap {
        let mut map = TerritoryMap::new();
        map.insert(
            "A".to_string(),
            make_territory(
                &["B", "C"],
                Resources {
                    ore: 7_200,
                    emeralds: 18_000,
                    ..Resources::default()
                },
            ),
        );
        map.insert(
            "B".to_string(),
            make_territory(
                &["A", "D"],
                Resources {
                    fish: 1,
                    wood: 1,
                    crops: 1,
                    ore: 1,
                    emeralds: 1,
                },
            ),
        );
        map.insert(
            "C".to_string(),
            make_territory(
                &["A", "D"],
                Resources {
                    fish: 7_200,
                    ..Resources::default()
                },
            ),
        );
        map.insert(
            "D".to_string(),
            make_territory(
                &["B", "C"],
                Resources {
                    wood: 1,
                    ..Resources::default()
                },
            ),
        );
        map
    }

    #[test]
    fn claim_document_round_trip_serializes_versioned_schema() {
        let mut document = ClaimDocumentV1::blank();
        document.title = Some("Example".to_string());
        document
            .overrides
            .insert("A".to_string(), guild("g1", "Alpha", "ALP"));
        document.macros.push(ClaimMacro {
            id: "macro-1".to_string(),
            name: "North".to_string(),
            territories: vec!["A".to_string(), "B".to_string()],
        });

        let encoded = serde_json::to_string(&document).expect("serialize");
        let decoded: ClaimDocumentV1 = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded.version, CLAIM_DOCUMENT_VERSION_V1);
        assert_eq!(decoded, document);
    }

    #[test]
    fn validate_claim_document_rejects_unknown_territories_and_duplicate_macro_ids() {
        let mut document = ClaimDocumentV1::blank();
        document
            .overrides
            .insert("Missing".to_string(), ClaimOwner::Neutral);
        assert_eq!(
            validate_claim_document(&document, ["A", "B"]),
            Err(ClaimValidationError::UnknownTerritory(
                "Missing".to_string()
            ))
        );

        document.overrides.clear();
        document.macros = vec![
            ClaimMacro {
                id: "dup".to_string(),
                name: "One".to_string(),
                territories: vec!["A".to_string()],
            },
            ClaimMacro {
                id: "dup".to_string(),
                name: "Two".to_string(),
                territories: vec!["B".to_string()],
            },
        ];
        assert_eq!(
            validate_claim_document(&document, ["A", "B"]),
            Err(ClaimValidationError::DuplicateMacroId("dup".to_string()))
        );
    }

    #[test]
    fn materialize_claim_owners_uses_blank_or_frozen_base_and_overrides() {
        let territories = sample_map();
        let mut document = ClaimDocumentV1::blank();
        document
            .overrides
            .insert("A".to_string(), guild("g1", "Alpha", "ALP"));

        let blank = materialize_claim_owners(&document, &territories);
        assert_eq!(blank.get("A"), Some(&guild("g1", "Alpha", "ALP")));
        assert_eq!(blank.get("B"), Some(&ClaimOwner::Neutral));

        let mut frozen_owners = HashMap::new();
        frozen_owners.insert("B".to_string(), guild("g2", "Beta", "BET"));
        let mut frozen = ClaimDocumentV1::frozen_live(None, 12, frozen_owners);
        frozen
            .overrides
            .insert("A".to_string(), guild("g1", "Alpha", "ALP"));
        let materialized = materialize_claim_owners(&frozen, &territories);
        assert_eq!(materialized.get("A"), Some(&guild("g1", "Alpha", "ALP")));
        assert_eq!(materialized.get("B"), Some(&guild("g2", "Beta", "BET")));
    }

    #[test]
    fn compact_claim_overrides_removes_values_matching_base() {
        let territories = sample_map();
        let mut owners = HashMap::new();
        owners.insert("A".to_string(), guild("g1", "Alpha", "ALP"));
        let mut document = ClaimDocumentV1::frozen_live(None, 1, owners);
        document
            .overrides
            .insert("A".to_string(), guild("g1", "Alpha", "ALP"));
        document
            .overrides
            .insert("B".to_string(), guild("g2", "Beta", "BET"));

        let compacted = compact_claim_overrides(&document, &territories);
        assert!(!compacted.contains_key("A"));
        assert_eq!(compacted.get("B"), Some(&guild("g2", "Beta", "BET")));
    }

    #[test]
    fn compute_claim_metrics_counts_resources_and_hubs() {
        let territories = sample_map();
        let mut owners = HashMap::new();
        owners.insert("A".to_string(), guild("g1", "Alpha", "ALP"));
        owners.insert("B".to_string(), guild("g1", "Alpha", "ALP"));
        owners.insert("C".to_string(), guild("g1", "Alpha", "ALP"));
        owners.insert("D".to_string(), guild("g2", "Beta", "BET"));
        let document = ClaimDocumentV1::frozen_live(None, 7, owners);

        let metrics = compute_claim_metrics(&document, &territories);
        assert_eq!(metrics.total_territories, 4);
        assert_eq!(metrics.neutral_territories, 0);
        assert_eq!(metrics.guilds.len(), 2);

        let alpha = metrics
            .guilds
            .iter()
            .find(|entry| entry.owner.display_name() == "Alpha")
            .expect("alpha metrics");
        assert_eq!(alpha.territory_count, 3);
        assert_eq!(alpha.resources.rainbow, 1);
        assert_eq!(alpha.resources.any_double, 2);
        assert_eq!(alpha.resources.double_ore, 1);
        assert_eq!(alpha.resources.double_emerald, 1);
        assert_eq!(
            alpha
                .top_by_connections
                .as_ref()
                .map(|hub| hub.territory.as_str()),
            Some("A")
        );
        assert_eq!(
            alpha.top_by_externals.as_ref().map(|hub| hub.externals),
            Some(2)
        );
    }
}
