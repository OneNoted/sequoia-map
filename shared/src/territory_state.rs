//! A territory's state as the official API supplies it, kept as supplied so history can
//! reproduce it, and the one mapping from that state onto the map's [`Territory`] parts.

use serde::{Deserialize, Serialize};

use crate::ingest::{DataProvenance, TerritoryRuntimeData, VisibilityClass};
use crate::territory::{Region, Resources};

/// Source name of runtime built from the official API.
pub const OFFICIAL_API_SOURCE: &str = "wynncraft_api";

/// Everything the official territory list supplies for a territory, with the holding it was
/// supplied for. `None` means the payload did not supply the value, never a default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TerritoryState {
    /// The holding the API reported this state for: the rest describes that holding only.
    pub owner: Option<StateOwner>,
    pub headquarters: Option<bool>,
    /// The owning guild's headquarters territory (`guild.hq`).
    pub guild_headquarters: Option<String>,
    pub treasury: Option<String>,
    pub defences: Option<String>,
    pub generation: Option<Resources>,
    pub base_generation: Option<Resources>,
    pub stored: Option<Resources>,
    pub capacity: Option<Resources>,
    pub location: Option<Region>,
    pub links: Option<Vec<String>>,
}

/// A holding: the owning guild and when it acquired the territory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateOwner {
    pub guild_uuid: String,
    pub acquired_micros: i64,
}

/// One field of a [`TerritoryState`] with its new value; `None` means it became unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerritoryStateField {
    Owner(Option<StateOwner>),
    Headquarters(Option<bool>),
    GuildHeadquarters(Option<String>),
    Treasury(Option<String>),
    Defences(Option<String>),
    Generation(Option<Resources>),
    BaseGeneration(Option<Resources>),
    Stored(Option<Resources>),
    Capacity(Option<Resources>),
    Location(Option<Region>),
    Links(Option<Vec<String>>),
}

macro_rules! fields {
    ($($field:ident => $variant:ident),* $(,)?) => {
        impl TerritoryState {
            /// The fields of `self` that differ from `before`, with their values in `self`.
            pub fn changes_from(&self, before: &Self) -> Vec<TerritoryStateField> {
                let mut changes = Vec::new();
                $(if self.$field != before.$field {
                    changes.push(TerritoryStateField::$variant(self.$field.clone()));
                })*
                changes
            }

            /// Every field with its value, `None` ones included: applied to any state, it
            /// replaces that state entirely.
            pub fn fields(&self) -> Vec<TerritoryStateField> {
                vec![$(TerritoryStateField::$variant(self.$field.clone())),*]
            }

            pub fn apply(&mut self, field: TerritoryStateField) {
                match field {
                    $(TerritoryStateField::$variant(value) => self.$field = value,)*
                }
            }
        }
    };
}

fields! {
    owner => Owner,
    headquarters => Headquarters,
    guild_headquarters => GuildHeadquarters,
    treasury => Treasury,
    defences => Defences,
    generation => Generation,
    base_generation => BaseGeneration,
    stored => Stored,
    capacity => Capacity,
    location => Location,
    links => Links,
}

impl TerritoryState {
    /// Whether the API reported this state for the holding of `guild_uuid` acquired at
    /// `acquired_micros`. Only then does its [`runtime`](Self::runtime) describe that holding:
    /// ownership from reporters can lead the API, and a new holding must not show the previous
    /// holder's headquarters, treasury, rates or stockpiles. Where the territory is, its links
    /// and what it produces ([`resources`](Self::resources)) do not depend on the owner.
    pub fn reported_for(&self, guild_uuid: &str, acquired_micros: i64) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.guild_uuid == guild_uuid && owner.acquired_micros == acquired_micros
        })
    }

    /// Resource production as the map shows it: the base rate of each resource the
    /// territory produces. The API reports a base rate for every slot, so a slot counts only
    /// when its current generation is positive.
    pub fn resources(&self) -> Resources {
        let Some(base) = &self.base_generation else {
            return Resources::default();
        };
        let Some(generation) = &self.generation else {
            return base.clone();
        };
        let produced = |base: i32, generation: i32| if generation > 0 { base } else { 0 };
        Resources {
            emeralds: produced(base.emeralds, generation.emeralds),
            ore: produced(base.ore, generation.ore),
            crops: produced(base.crops, generation.crops),
            fish: produced(base.fish, generation.fish),
            wood: produced(base.wood, generation.wood),
        }
    }

    pub fn connections(&self) -> Vec<String> {
        self.links.clone().unwrap_or_default()
    }

    /// Runtime as the map shows it, observed at `observed_at` (RFC 3339).
    pub fn runtime(&self, observed_at: String) -> Option<TerritoryRuntimeData> {
        let non_empty = |resources: &Option<Resources>| resources.clone().filter(|r| !r.is_empty());
        let runtime = TerritoryRuntimeData {
            headquarters: self.headquarters,
            headquarters_territory: self
                .guild_headquarters
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            held_resources: non_empty(&self.stored),
            production_rates: non_empty(&self.generation),
            storage_capacity: non_empty(&self.capacity),
            treasury: tier_label(self.treasury.as_deref()),
            defense_tier: tier_label(self.defences.as_deref()),
            contested: None,
            active_war: None,
            extra_scrapes: None,
            provenance: None,
        };
        if runtime == TerritoryRuntimeData::default() {
            return None;
        }
        Some(TerritoryRuntimeData {
            provenance: Some(DataProvenance {
                source: OFFICIAL_API_SOURCE.to_string(),
                visibility: VisibilityClass::Public,
                confidence: 1.0,
                reporter_count: 1,
                observed_at,
                ..DataProvenance::default()
            }),
            ..runtime
        })
    }
}

/// The API writes tiers as `VERY_HIGH`; the map shows `VERY HIGH`.
fn tier_label(raw: Option<&str>) -> Option<String> {
    raw.map(|value| value.trim().replace('_', " "))
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(emeralds: i32, ore: i32) -> Resources {
        Resources {
            emeralds,
            ore,
            ..Resources::default()
        }
    }

    #[test]
    fn applying_the_changes_from_a_state_reproduces_it() {
        let before = TerritoryState {
            headquarters: Some(true),
            treasury: Some("LOW".into()),
            stored: Some(resources(10, 0)),
            links: Some(vec!["A".into()]),
            ..TerritoryState::default()
        };
        let after = TerritoryState {
            headquarters: Some(false),
            treasury: None,
            stored: Some(resources(12, 3)),
            links: Some(vec!["A".into()]),
            defences: Some("HIGH".into()),
            ..TerritoryState::default()
        };
        let changes = after.changes_from(&before);
        assert_eq!(
            changes.len(),
            4,
            "unchanged fields are not repeated: {changes:?}"
        );
        let mut rebuilt = before;
        for change in changes {
            rebuilt.apply(change);
        }
        assert_eq!(rebuilt, after);
    }

    #[test]
    fn all_fields_replace_a_state_including_the_ones_that_are_unknown() {
        let mut stale = TerritoryState {
            treasury: Some("HIGH".into()),
            stored: Some(resources(7, 7)),
            ..TerritoryState::default()
        };
        let current = TerritoryState {
            headquarters: Some(true),
            ..TerritoryState::default()
        };
        for field in current.fields() {
            stale.apply(field);
        }
        assert_eq!(stale, current);
    }

    #[test]
    fn another_holding_keeps_what_the_territory_produces_but_none_of_the_runtime() {
        let state = TerritoryState {
            owner: Some(StateOwner {
                guild_uuid: "a".into(),
                acquired_micros: 10,
            }),
            headquarters: Some(true),
            treasury: Some("MEDIUM".into()),
            generation: Some(resources(9000, 0)),
            base_generation: Some(resources(9000, 3600)),
            stored: Some(resources(5, 5)),
            ..TerritoryState::default()
        };
        assert!(state.reported_for("a", 10));
        assert!(!state.reported_for("b", 10));
        assert!(!state.reported_for("a", 11));
        assert!(!TerritoryState::default().reported_for("a", 10));
        assert_eq!(
            state.resources(),
            resources(9000, 0),
            "ore is not produced here"
        );
    }

    #[test]
    fn the_map_counts_only_produced_resources_and_shows_spaced_tiers() {
        let state = TerritoryState {
            generation: Some(resources(9359, 0)),
            base_generation: Some(resources(9000, 3600)),
            stored: Some(Resources::default()),
            defences: Some("VERY_HIGH".into()),
            ..TerritoryState::default()
        };
        assert_eq!(state.resources(), resources(9000, 0));
        let runtime = state
            .runtime("2026-11-02T11:16:38Z".into())
            .expect("runtime");
        assert_eq!(runtime.defense_tier.as_deref(), Some("VERY HIGH"));
        assert_eq!(
            runtime.held_resources, None,
            "an empty stockpile shows as none"
        );
        assert_eq!(
            runtime.provenance.map(|p| (p.source, p.observed_at)),
            Some((OFFICIAL_API_SOURCE.into(), "2026-11-02T11:16:38Z".into()))
        );
        assert_eq!(TerritoryState::default().runtime(String::new()), None);
    }
}
