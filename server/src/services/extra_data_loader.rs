use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use sequoia_shared::Resources;
use tracing::{info, warn};

use crate::config::{TERREXTRA_REFRESH_SECS, territory_extra_url};
use crate::state::{AppState, ExtraTerrInfo};

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(TERREXTRA_REFRESH_SECS));

    // Fetch immediately on startup, then hourly
    loop {
        interval.tick().await;

        match load_extra_data(&state.http_client).await {
            Ok(data) => {
                let count = data.len();
                *state.extra_terr.write().await = data;
                state.extra_data_dirty.store(true, Ordering::Release);
                info!("loaded extra territory data for {count} territories");
            }
            Err(e) => {
                let mut data = state.extra_terr.write().await;
                merge_extra_data(&mut data, bundled_extra_data());
                sanitize_extra_data(&mut data);
                state.extra_data_dirty.store(true, Ordering::Release);
                warn!(
                    "failed to fetch supplemental territory data; preserved cached data and refreshed bundled defaults: {e}"
                );
            }
        }
    }
}

async fn load_extra_data(
    client: &reqwest::Client,
) -> Result<HashMap<String, ExtraTerrInfo>, reqwest::Error> {
    let mut data = match territory_extra_url() {
        Some(url) => fetch_extra_data(client, &url).await?,
        None => HashMap::new(),
    };

    merge_extra_data(&mut data, bundled_extra_data());
    sanitize_extra_data(&mut data);
    Ok(data)
}

async fn fetch_extra_data(
    client: &reqwest::Client,
    url: &str,
) -> Result<HashMap<String, ExtraTerrInfo>, reqwest::Error> {
    let resp = client.get(url).send().await?.error_for_status()?;
    resp.json().await
}

fn merge_extra_data(
    target: &mut HashMap<String, ExtraTerrInfo>,
    source: HashMap<String, ExtraTerrInfo>,
) {
    for (name, info) in source {
        let entry = target.entry(name.clone()).or_default();
        if entry.resources.is_empty() && !info.resources.is_empty() {
            entry.resources = info.resources;
        }
        merge_connections(&name, &mut entry.connections, info.connections);
    }
}

fn merge_connections(
    territory_name: &str,
    target: &mut Vec<String>,
    source: impl IntoIterator<Item = String>,
) {
    for connection in source {
        if connection == territory_name || target.iter().any(|existing| existing == &connection) {
            continue;
        }
        target.push(connection);
    }
}

fn sanitize_extra_data(data: &mut HashMap<String, ExtraTerrInfo>) {
    for (name, info) in data {
        let mut deduped = Vec::with_capacity(info.connections.len());
        for connection in info.connections.drain(..) {
            if connection == *name || deduped.iter().any(|existing| existing == &connection) {
                continue;
            }
            deduped.push(connection);
        }
        info.connections = deduped;
    }
}

struct BundledTerritoryExtra {
    name: &'static str,
    resources: Resources,
    connections: &'static [&'static str],
}

fn bundled_extra_data() -> HashMap<String, ExtraTerrInfo> {
    bundled_territory_extra_data()
        .into_iter()
        .map(|extra| {
            (
                extra.name.to_string(),
                ExtraTerrInfo {
                    resources: extra.resources,
                    connections: extra
                        .connections
                        .iter()
                        .filter(|connection| **connection != extra.name)
                        .map(|connection| (*connection).to_string())
                        .collect(),
                },
            )
        })
        .collect()
}

fn single_resource(resource: &str) -> Resources {
    let mut resources = Resources {
        emeralds: 9_000,
        ..Resources::default()
    };
    match resource {
        "ore" => resources.ore = 3_600,
        "crops" => resources.crops = 3_600,
        "fish" => resources.fish = 3_600,
        "wood" => resources.wood = 3_600,
        _ => {}
    }
    resources
}

fn single_resource_with_emeralds(resource: &str, emeralds: i32) -> Resources {
    let mut resources = single_resource(resource);
    resources.emeralds = emeralds;
    resources
}

fn all_resources() -> Resources {
    Resources {
        emeralds: 1_800,
        ore: 900,
        crops: 900,
        fish: 900,
        wood: 900,
    }
}

fn royal_gate_resources() -> Resources {
    Resources {
        emeralds: 9_000,
        fish: 7_200,
        ..Resources::default()
    }
}

fn bundled_territory_extra_data() -> [BundledTerritoryExtra; 32] {
    [
        BundledTerritoryExtra {
            name: "Royal Gate",
            resources: royal_gate_resources(),
            connections: &["Lighthouse Lookout", "Fort Torann"],
        },
        BundledTerritoryExtra {
            name: "Wellspring of Eternity",
            resources: all_resources(),
            connections: &["Fort Torann", "The Frog Bog"],
        },
        BundledTerritoryExtra {
            name: "Fort Torann",
            resources: single_resource("ore"),
            connections: &[
                "Royal Gate",
                "Wellspring of Eternity",
                "Xima Valley",
                "Forts in Fall",
            ],
        },
        BundledTerritoryExtra {
            name: "Xima Valley",
            resources: single_resource("fish"),
            connections: &["Fort Torann"],
        },
        BundledTerritoryExtra {
            name: "The Frog Bog",
            resources: single_resource("fish"),
            connections: &[
                "Wellspring of Eternity",
                "Forts in Fall",
                "Festival Grounds",
            ],
        },
        BundledTerritoryExtra {
            name: "Forts in Fall",
            resources: single_resource("wood"),
            connections: &[
                "Fort Torann",
                "Royal Dam",
                "The Frog Bog",
                "Espren",
                "The Lumbermill",
            ],
        },
        BundledTerritoryExtra {
            name: "Royal Dam",
            resources: single_resource("wood"),
            connections: &["Forts in Fall", "Lake Gitephe"],
        },
        BundledTerritoryExtra {
            name: "Festival Grounds",
            resources: single_resource("wood"),
            connections: &["The Frog Bog", "Espren"],
        },
        BundledTerritoryExtra {
            name: "Espren",
            resources: single_resource_with_emeralds("wood", 18_000),
            connections: &[
                "Forts in Fall",
                "Festival Grounds",
                "Agricultural Sector",
                "The Lumbermill",
            ],
        },
        BundledTerritoryExtra {
            name: "The Lumbermill",
            resources: single_resource("wood"),
            connections: &[
                "Forts in Fall",
                "Espren",
                "Gates to Aelumia",
                "Royal Barracks",
            ],
        },
        BundledTerritoryExtra {
            name: "Timasca",
            resources: single_resource("ore"),
            connections: &[
                "University Campus",
                "Residence Sector",
                "Industrial Sector",
                "Deforested Ecotone",
            ],
        },
        BundledTerritoryExtra {
            name: "Residence Sector",
            resources: single_resource("crops"),
            connections: &["Timasca", "Agricultural Sector"],
        },
        BundledTerritoryExtra {
            name: "Agricultural Sector",
            resources: single_resource("crops"),
            connections: &[
                "Espren",
                "Residence Sector",
                "Water Processing Sector",
                "Industrial Sector",
            ],
        },
        BundledTerritoryExtra {
            name: "Water Processing Sector",
            resources: single_resource("crops"),
            connections: &["Industrial Sector", "Agricultural Sector"],
        },
        BundledTerritoryExtra {
            name: "Industrial Sector",
            resources: single_resource("fish"),
            connections: &["Timasca", "Agricultural Sector", "Water Processing Sector"],
        },
        BundledTerritoryExtra {
            name: "Highlands Gate",
            resources: single_resource("wood"),
            connections: &["Verdant Grove", "Alder Understory"],
        },
        BundledTerritoryExtra {
            name: "Alder Understory",
            resources: single_resource("wood"),
            connections: &["Fort Tericen", "Aldwell", "Highlands Gate", "Fort Hegea"],
        },
        BundledTerritoryExtra {
            name: "Verdant Grove",
            resources: single_resource("wood"),
            connections: &[
                "Contested District",
                "Aldwell",
                "Deforested Ecotone",
                "Highlands Gate",
            ],
        },
        BundledTerritoryExtra {
            name: "Aldwell",
            resources: single_resource_with_emeralds("crops", 18_000),
            connections: &[
                "Deforested Ecotone",
                "Fort Hegea",
                "Alder Understory",
                "Verdant Grove",
            ],
        },
        BundledTerritoryExtra {
            name: "Fort Hegea",
            resources: all_resources(),
            connections: &["Alder Understory", "Aldwell"],
        },
        BundledTerritoryExtra {
            name: "Deforested Ecotone",
            resources: single_resource("wood"),
            connections: &["Timasca", "Verdant Grove", "Aldwell"],
        },
        BundledTerritoryExtra {
            name: "Citadel's Shadow",
            resources: single_resource("crops"),
            connections: &["Lake Gitephe", "Gates to Aelumia", "Fort Tericen"],
        },
        BundledTerritoryExtra {
            name: "Lake Gitephe",
            resources: single_resource("fish"),
            connections: &["Royal Dam", "Citadel's Shadow", "Hyloch"],
        },
        BundledTerritoryExtra {
            name: "Hyloch",
            resources: single_resource_with_emeralds("crops", 18_000),
            connections: &["Lake Gitephe", "Fort Tericen", "Lake Rieke"],
        },
        BundledTerritoryExtra {
            name: "Fort Tericen",
            resources: single_resource("wood"),
            connections: &[
                "Alder Understory",
                "Citadel's Shadow",
                "Hyloch",
                "Lake Rieke",
                "Feuding Houses",
            ],
        },
        BundledTerritoryExtra {
            name: "Lake Rieke",
            resources: single_resource("fish"),
            connections: &["Hyloch", "Frosty Outpost", "Fort Tericen"],
        },
        BundledTerritoryExtra {
            name: "Frosty Outpost",
            resources: single_resource("ore"),
            connections: &["Lake Rieke", "Feuding Houses"],
        },
        BundledTerritoryExtra {
            name: "Feuding Houses",
            resources: single_resource("ore"),
            connections: &["Frosty Outpost", "Fort Tericen"],
        },
        BundledTerritoryExtra {
            name: "Royal Barracks",
            resources: single_resource("wood"),
            connections: &["The Lumbermill", "Gates to Aelumia", "University Campus"],
        },
        BundledTerritoryExtra {
            name: "Gates to Aelumia",
            resources: single_resource("fish"),
            connections: &[
                "Royal Barracks",
                "Citadel's Shadow",
                "Contested District",
                "The Lumbermill",
            ],
        },
        BundledTerritoryExtra {
            name: "Contested District",
            resources: single_resource("ore"),
            connections: &["Verdant Grove", "University Campus", "Gates to Aelumia"],
        },
        BundledTerritoryExtra {
            name: "University Campus",
            resources: single_resource("crops"),
            connections: &["Timasca", "Contested District", "Royal Barracks"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_extra_data_includes_fruma_resources() {
        let data = bundled_extra_data();

        assert_eq!(data["Forts in Fall"].resources.wood, 3_600);
        assert_eq!(data["Royal Dam"].resources.wood, 3_600);
        assert_eq!(data["Espren"].resources.emeralds, 18_000);
        assert_eq!(data["Aldwell"].resources.emeralds, 18_000);
        assert_eq!(data["Hyloch"].resources.emeralds, 18_000);
        assert_eq!(data["Wellspring of Eternity"].resources.ore, 900);
        assert_eq!(data["Wellspring of Eternity"].resources.crops, 900);
        assert_eq!(data["Fort Hegea"].resources.fish, 900);
    }

    #[test]
    fn bundled_extra_data_includes_fruma_connections() {
        let data = bundled_extra_data();

        assert_eq!(
            data["Fort Torann"].connections,
            [
                "Royal Gate",
                "Wellspring of Eternity",
                "Xima Valley",
                "Forts in Fall"
            ]
        );
        assert_eq!(
            data["Forts in Fall"].connections,
            [
                "Fort Torann",
                "Royal Dam",
                "The Frog Bog",
                "Espren",
                "The Lumbermill"
            ]
        );
        assert!(
            data["Royal Gate"]
                .connections
                .contains(&"Fort Torann".to_string())
        );
    }

    #[test]
    fn bundled_extra_data_has_no_self_connections() {
        let data = bundled_extra_data();

        for (name, info) in &data {
            assert!(
                !info.connections.iter().any(|connection| connection == name),
                "{name} should not connect to itself"
            );
        }
    }

    #[test]
    fn bundled_extra_data_targets_known_territories() {
        let data = bundled_extra_data();
        let known_external_targets = ["Lighthouse Lookout"];

        for (name, info) in &data {
            for connection in &info.connections {
                assert!(
                    data.contains_key(connection)
                        || known_external_targets.contains(&connection.as_str()),
                    "{name} has unknown bundled connection target {connection}"
                );
            }
        }
    }

    #[test]
    fn bundled_extra_data_connections_are_reciprocal_inside_bundle() {
        let data = bundled_extra_data();

        for (name, info) in &data {
            for connection in &info.connections {
                let Some(connected_info) = data.get(connection) else {
                    continue;
                };
                assert!(
                    connected_info.connections.contains(name),
                    "{name} -> {connection} should have a reciprocal bundled edge"
                );
            }
        }
    }

    #[test]
    fn bundled_extra_data_supports_fruma_external_traversal() {
        let data = bundled_extra_data();
        let connections_map = data
            .iter()
            .map(|(name, info)| (name.clone(), info.connections.clone()))
            .collect();

        let externals = sequoia_shared::tower::find_externals("Fort Torann", &connections_map, 3);

        assert!(externals.contains("Royal Gate"));
        assert!(externals.contains("Royal Dam"));
        assert!(externals.contains("Festival Grounds"));
    }

    #[test]
    fn merge_extra_data_fills_missing_resources_and_keeps_connections() {
        let mut target = HashMap::from([(
            "Forts in Fall".to_string(),
            ExtraTerrInfo {
                resources: Resources::default(),
                connections: vec!["Royal Dam".to_string()],
            },
        )]);
        let source = HashMap::from([(
            "Forts in Fall".to_string(),
            ExtraTerrInfo {
                resources: single_resource("wood"),
                connections: Vec::new(),
            },
        )]);

        merge_extra_data(&mut target, source);

        let merged = &target["Forts in Fall"];
        assert_eq!(merged.resources.wood, 3_600);
        assert_eq!(merged.connections, ["Royal Dam"]);
    }

    #[test]
    fn merge_extra_data_preserves_existing_remote_resources() {
        let mut target = HashMap::from([(
            "Forts in Fall".to_string(),
            ExtraTerrInfo {
                resources: single_resource("ore"),
                connections: vec!["Royal Dam".to_string()],
            },
        )]);
        let source = HashMap::from([(
            "Forts in Fall".to_string(),
            ExtraTerrInfo {
                resources: single_resource("wood"),
                connections: Vec::new(),
            },
        )]);

        merge_extra_data(&mut target, source);

        let merged = &target["Forts in Fall"];
        assert_eq!(merged.resources.ore, 3_600);
        assert_eq!(merged.resources.wood, 0);
        assert_eq!(merged.connections, ["Royal Dam"]);
    }

    #[test]
    fn merge_extra_data_merges_connections_without_duplicates_or_self_edges() {
        let mut target = HashMap::from([(
            "Industrial Sector".to_string(),
            ExtraTerrInfo {
                resources: Resources::default(),
                connections: vec!["Timasca".to_string()],
            },
        )]);
        let source = HashMap::from([(
            "Industrial Sector".to_string(),
            ExtraTerrInfo {
                resources: Resources::default(),
                connections: vec![
                    "Timasca".to_string(),
                    "Industrial Sector".to_string(),
                    "Water Processing Sector".to_string(),
                ],
            },
        )]);

        merge_extra_data(&mut target, source);

        assert_eq!(
            target["Industrial Sector"].connections,
            ["Timasca", "Water Processing Sector"]
        );
    }
}
