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
        let entry = target.entry(name).or_default();
        if entry.resources.is_empty() && !info.resources.is_empty() {
            entry.resources = info.resources;
        }
        if !info.connections.is_empty() {
            entry.connections = info.connections;
        }
    }
}

fn bundled_extra_data() -> HashMap<String, ExtraTerrInfo> {
    fruma_resource_data()
        .into_iter()
        .map(|(name, resources)| {
            (
                name.to_string(),
                ExtraTerrInfo {
                    resources,
                    connections: Vec::new(),
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

fn all_resources() -> Resources {
    Resources {
        emeralds: 1_800,
        ore: 900,
        crops: 900,
        fish: 900,
        wood: 900,
    }
}

fn fruma_resource_data() -> [(&'static str, Resources); 31] {
    [
        ("Wellspring of Eternity", all_resources()),
        ("Fort Torann", single_resource("ore")),
        ("Xima Valley", single_resource("fish")),
        ("The Frog Bog", single_resource("fish")),
        ("Forts in Fall", single_resource("wood")),
        ("Royal Dam", single_resource("wood")),
        ("Festival Grounds", single_resource("wood")),
        ("Espren", single_resource("wood")),
        ("The Lumbermill", single_resource("wood")),
        ("Timasca", single_resource("ore")),
        ("Residence Sector", single_resource("crops")),
        ("Agricultural Sector", single_resource("crops")),
        ("Water Processing Sector", single_resource("crops")),
        ("Industrial Sector", single_resource("fish")),
        ("Highlands Gate", single_resource("wood")),
        ("Alder Understory", single_resource("wood")),
        ("Verdant Grove", single_resource("wood")),
        ("Aldwell", single_resource("crops")),
        ("Fort Hegea", all_resources()),
        ("Deforested Ecotone", single_resource("wood")),
        ("Citadel's Shadow", single_resource("crops")),
        ("Lake Gitephe", single_resource("fish")),
        ("Hyloch", single_resource("crops")),
        ("Fort Tericen", single_resource("wood")),
        ("Lake Rieke", single_resource("fish")),
        ("Frosty Outpost", single_resource("ore")),
        ("Feuding Houses", single_resource("ore")),
        ("Royal Barracks", single_resource("wood")),
        ("Gates to Aelumia", single_resource("fish")),
        ("Contested District", single_resource("ore")),
        ("University Campus", single_resource("crops")),
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
        assert_eq!(data["Wellspring of Eternity"].resources.ore, 900);
        assert_eq!(data["Wellspring of Eternity"].resources.crops, 900);
        assert_eq!(data["Fort Hegea"].resources.fish, 900);
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
}
