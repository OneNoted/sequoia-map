//! What an export shows, derived from the public live territory map.

use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use sequoia_shared::{Territory, TerritoryMap, guild_color};

/// Everything drawn on a map export; two equal scenes produce identical images.
///
/// Built only from the public live territory map, so it cannot carry Intel, player
/// positions or any other signed-in overlay.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MapScene {
    as_of: DateTime<Utc>,
    guilds: Vec<Guild>,
    territories: Vec<Plot>,
    connections: Vec<[i32; 4]>,
}

/// A guild with at least one territory, in legend order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Guild {
    pub(super) prefix: String,
    pub(super) name: String,
    pub(super) color: [u8; 3],
    pub(super) territories: u32,
}

/// One territory: inclusive world bounds, owner index into the guild list
/// (`None` when unclaimed), hold-time label and headquarters flag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Plot {
    pub(super) bounds: [i32; 4],
    pub(super) owner: Option<usize>,
    pub(super) held: String,
    pub(super) headquarters: bool,
}

impl MapScene {
    /// Scene for `territories` with hold times measured at `now`.
    ///
    /// The clock is truncated to the minute: hold times are shown to the minute at most,
    /// so every request within a minute shares one image.
    pub fn new(territories: &TerritoryMap, now: DateTime<Utc>) -> Self {
        let as_of = now.duration_trunc(TimeDelta::minutes(1)).unwrap_or(now);
        let mut named: Vec<(&String, &Territory)> = territories.iter().collect();
        named.sort_unstable_by(|left, right| left.0.cmp(right.0));

        let guilds = legend(&named);
        let guild_index: BTreeMap<&str, usize> = guilds
            .iter()
            .enumerate()
            .map(|(index, guild)| (guild.name.as_str(), index))
            .collect();

        let plots = named
            .iter()
            .map(|(_, territory)| {
                let location = &territory.location;
                Plot {
                    bounds: [
                        location.left(),
                        location.top(),
                        location.right(),
                        location.bottom(),
                    ],
                    owner: owner_name(territory).and_then(|name| guild_index.get(name).copied()),
                    held: held_label((as_of - territory.acquired).num_seconds()),
                    headquarters: territory
                        .runtime
                        .as_ref()
                        .and_then(|runtime| runtime.headquarters)
                        .unwrap_or(false),
                }
            })
            .collect();

        Self {
            as_of,
            guilds,
            territories: plots,
            connections: connections(territories, &named),
        }
    }

    /// The instant hold times are measured from, as printed on the image.
    pub fn as_of(&self) -> DateTime<Utc> {
        self.as_of
    }

    /// Stable digest of everything the image shows.
    pub(super) fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    pub(super) fn guilds(&self) -> &[Guild] {
        &self.guilds
    }

    pub(super) fn territories(&self) -> &[Plot] {
        &self.territories
    }

    /// Connection segments between territory centres, as world `[x1, z1, x2, z2]`.
    pub(super) fn connections(&self) -> &[[i32; 4]] {
        &self.connections
    }

    pub(super) fn territory_count(&self) -> usize {
        self.territories.len()
    }
}

fn owner_name(territory: &Territory) -> Option<&str> {
    let name = territory.guild.name.trim();
    (!territory.guild.is_unclaimed() && !name.is_empty()).then_some(name)
}

/// Guilds by territory count (largest first), then name.
fn legend(named: &[(&String, &Territory)]) -> Vec<Guild> {
    let mut guilds: BTreeMap<&str, Guild> = BTreeMap::new();
    for (_, territory) in named {
        let Some(name) = owner_name(territory) else {
            continue;
        };
        let guild = &territory.guild;
        guilds
            .entry(name)
            .or_insert_with(|| {
                let (r, g, b) = guild.color.unwrap_or_else(|| guild_color(&guild.name));
                Guild {
                    prefix: guild.prefix.trim().to_string(),
                    name: name.to_string(),
                    color: [r, g, b],
                    territories: 0,
                }
            })
            .territories += 1;
    }
    let mut guilds: Vec<Guild> = guilds.into_values().collect();
    guilds.sort_by(|left, right| {
        right
            .territories
            .cmp(&left.territories)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    guilds
}

fn connections(territories: &TerritoryMap, named: &[(&String, &Territory)]) -> Vec<[i32; 4]> {
    let mut seen = HashSet::new();
    let mut segments = Vec::new();
    for (name, territory) in named {
        for neighbour in &territory.connections {
            let Some(other) = territories.get(neighbour) else {
                continue;
            };
            let pair = if name.as_str() < neighbour.as_str() {
                (name.as_str(), neighbour.as_str())
            } else {
                (neighbour.as_str(), name.as_str())
            };
            if pair.0 == pair.1 || !seen.insert(pair) {
                continue;
            }
            let (from, to) = (&territory.location, &other.location);
            segments.push([
                from.midpoint_x(),
                from.midpoint_y(),
                to.midpoint_x(),
                to.midpoint_y(),
            ]);
        }
    }
    segments
}

/// Compact hold time, coarse enough to stay fixed for at least a minute.
fn held_label(seconds: i64) -> String {
    match seconds.max(0) {
        seconds if seconds < 60 => "now".to_string(),
        seconds if seconds < 3_600 => format!("{}m", seconds / 60),
        seconds if seconds < 86_400 => format!("{}h", seconds / 3_600),
        seconds if seconds < 604_800 => format!("{}d", seconds / 86_400),
        seconds => format!("{}w", seconds / 604_800),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use chrono::TimeZone;
    use sequoia_shared::{GuildRef, Region, TerritoryRuntimeData};

    use super::*;

    pub(crate) fn territory(guild: &str, prefix: &str, acquired: DateTime<Utc>) -> Territory {
        Territory {
            guild: GuildRef {
                uuid: format!("{guild}-uuid"),
                name: guild.to_string(),
                prefix: prefix.to_string(),
                color: Some((10, 200, 30)),
            },
            acquired,
            location: Region {
                start: [-200, -3000],
                end: [-100, -2900],
            },
            resources: Default::default(),
            connections: Vec::new(),
            runtime: None,
        }
    }

    fn clock() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 11, 19, 26, 42).unwrap()
    }

    #[test]
    fn hold_times_age_with_the_request_clock() {
        // Hold times are measured from the clock truncated to the minute (19:26:00).
        let acquired = clock() - TimeDelta::minutes(59) - TimeDelta::seconds(42);
        let map =
            TerritoryMap::from([("Ragni".to_string(), territory("Sequoia", "SEQ", acquired))]);

        let earlier = MapScene::new(&map, clock());
        let later = MapScene::new(&map, clock() + TimeDelta::minutes(2));

        assert_eq!(earlier.territories()[0].held, "59m");
        assert_eq!(later.territories()[0].held, "1h");
        assert_ne!(earlier.fingerprint(), later.fingerprint());
    }

    #[test]
    fn requests_within_one_minute_share_a_scene() {
        let map = TerritoryMap::from([(
            "Ragni".to_string(),
            territory("Sequoia", "SEQ", clock() - TimeDelta::hours(3)),
        )]);

        let first = MapScene::new(&map, clock());
        let second = MapScene::new(&map, clock() + TimeDelta::seconds(17));

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn ownership_change_changes_the_fingerprint() {
        let acquired = clock() - TimeDelta::hours(3);
        let before =
            TerritoryMap::from([("Ragni".to_string(), territory("Sequoia", "SEQ", acquired))]);
        let after =
            TerritoryMap::from([("Ragni".to_string(), territory("Avicia", "AVO", acquired))]);

        assert_ne!(
            MapScene::new(&before, clock()).fingerprint(),
            MapScene::new(&after, clock()).fingerprint()
        );
    }

    #[test]
    fn legend_orders_guilds_by_territory_count_and_skips_unclaimed() {
        let acquired = clock() - TimeDelta::days(2);
        let mut unclaimed = territory("Unclaimed", "NONE", acquired);
        unclaimed.guild.uuid = "00000000-0000-0000-0000-000000000000".to_string();
        let map = TerritoryMap::from([
            ("A".to_string(), territory("Avicia", "AVO", acquired)),
            ("B".to_string(), territory("Sequoia", "SEQ", acquired)),
            ("C".to_string(), territory("Sequoia", "SEQ", acquired)),
            ("D".to_string(), unclaimed),
        ]);

        let scene = MapScene::new(&map, clock());

        let legend: Vec<(&str, u32)> = scene
            .guilds()
            .iter()
            .map(|guild| (guild.name.as_str(), guild.territories))
            .collect();
        assert_eq!(legend, [("Sequoia", 2), ("Avicia", 1)]);
        assert_eq!(scene.territories()[3].owner, None);
    }

    #[test]
    fn connections_are_drawn_once_per_pair() {
        let acquired = clock() - TimeDelta::days(2);
        let mut ragni = territory("Sequoia", "SEQ", acquired);
        ragni.connections = vec!["Detlas".to_string(), "Missing".to_string()];
        let mut detlas = territory("Sequoia", "SEQ", acquired);
        detlas.connections = vec!["Ragni".to_string()];
        let map =
            TerritoryMap::from([("Ragni".to_string(), ragni), ("Detlas".to_string(), detlas)]);

        assert_eq!(MapScene::new(&map, clock()).connections().len(), 1);
    }

    #[test]
    fn headquarters_move_changes_the_fingerprint() {
        let acquired = clock() - TimeDelta::hours(3);
        let plain =
            TerritoryMap::from([("Ragni".to_string(), territory("Sequoia", "SEQ", acquired))]);
        let mut hq = territory("Sequoia", "SEQ", acquired);
        hq.runtime = Some(TerritoryRuntimeData {
            headquarters: Some(true),
            ..Default::default()
        });
        let moved = TerritoryMap::from([("Ragni".to_string(), hq)]);

        assert_ne!(
            MapScene::new(&plain, clock()).fingerprint(),
            MapScene::new(&moved, clock()).fingerprint()
        );
    }

    #[test]
    fn runtime_details_that_are_not_drawn_keep_the_fingerprint() {
        let acquired = clock() - TimeDelta::hours(3);
        let before =
            TerritoryMap::from([("Ragni".to_string(), territory("Sequoia", "SEQ", acquired))]);
        let mut busy = territory("Sequoia", "SEQ", acquired);
        busy.runtime = Some(TerritoryRuntimeData {
            headquarters: Some(false),
            treasury: Some("VERY_HIGH".to_string()),
            defense_tier: Some("HIGH".to_string()),
            held_resources: Some(Default::default()),
            ..Default::default()
        });
        let after = TerritoryMap::from([("Ragni".to_string(), busy)]);

        assert_eq!(
            MapScene::new(&before, clock()).fingerprint(),
            MapScene::new(&after, clock()).fingerprint()
        );
    }

    #[test]
    fn headquarters_come_from_public_runtime_data() {
        let mut hq = territory("Sequoia", "SEQ", clock());
        hq.runtime = Some(TerritoryRuntimeData {
            headquarters: Some(true),
            ..Default::default()
        });
        let map = TerritoryMap::from([("Ragni".to_string(), hq)]);

        assert!(MapScene::new(&map, clock()).territories()[0].headquarters);
    }

    #[test]
    fn held_labels_use_the_coarsest_whole_unit() {
        let labels: Vec<String> = [0, 59, 60, 3_599, 3_600, 86_399, 86_400, 604_800, -5]
            .into_iter()
            .map(held_label)
            .collect();
        assert_eq!(
            labels,
            ["now", "now", "1m", "59m", "1h", "23h", "1d", "1w", "now"]
        );
    }
}
