use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use sequoia_shared::{
    GatheringNodeCollectionSummary, GatheringNodeMarker, MapActivityCollectionSummary,
    MapActivityMarker, MapActivitySummary, MapIntelOverlay, MapIntelSummary, MapPoint,
    MapRewardSummary, NamedCount, WorldEventCollectionSummary, WorldEventMarker, WorldEventSummary,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::config::{
    MAP_INTEL_CACHE_TTL_SECS, SEASON_LEADERBOARD_CACHE_TTL_SECS, WYNNCRAFT_GUILD_SEASONS_URL,
    WYNNCRAFT_LEADERBOARD_TYPES_URL, WYNNCRAFT_LEADERBOARDS_URL, WYNNCRAFT_MAP_CAMPS_URL,
    WYNNCRAFT_MAP_GATHERING_NODES_URL, WYNNCRAFT_MAP_RAIDS_URL, WYNNCRAFT_MAP_WORLD_EVENTS_URL,
};
use crate::state::{
    AppState, CachedMapIntel, CachedSeasonLeaderboard, CachedSeasonLeaderboardEntry,
};

#[derive(Debug, Clone)]
struct MapIntelPayload {
    summary: MapIntelSummary,
    overlay: MapIntelOverlay,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuildSeasonDefinition {
    #[serde(default, rename = "startDate", alias = "initDate")]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default, rename = "endDate")]
    pub end_date: Option<DateTime<Utc>>,
    #[serde(default, rename = "territoryHoldingSrPerHour")]
    pub territory_holding_sr_per_hour: Option<i32>,
    #[serde(default, rename = "srPerWar")]
    pub sr_per_war: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct RawLeaderboardEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    score: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMapActivity {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "internalName")]
    internal_name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(default)]
    level: Option<i32>,
    #[serde(default)]
    length: Option<String>,
    #[serde(default)]
    requirements: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    location: Option<MapPoint>,
    #[serde(default)]
    rewards: Vec<RawMapReward>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMapReward {
    #[serde(default)]
    always: Option<bool>,
    #[serde(default)]
    tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawWorldEvent {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "internalName")]
    internal_name: String,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(default)]
    level: Option<i32>,
    #[serde(default)]
    length: Option<String>,
    #[serde(default)]
    location: Vec<RawWorldEventLocation>,
    #[serde(default)]
    schedule: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawWorldEventLocation {
    #[serde(default)]
    event: Option<MapPoint>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawGatheringNode {
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    z: Option<f64>,
    #[serde(default)]
    angle: Option<f64>,
    #[serde(default, rename = "type")]
    node_type: String,
    #[serde(default)]
    resource: String,
    #[serde(default)]
    level: Option<i32>,
}

pub async fn fetch_guild_seasons(
    client: &reqwest::Client,
) -> Result<HashMap<String, GuildSeasonDefinition>, String> {
    client
        .get(WYNNCRAFT_GUILD_SEASONS_URL)
        .send()
        .await
        .map_err(|e| format!("guild seasons request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("guild seasons status: {e}"))?
        .json::<HashMap<String, GuildSeasonDefinition>>()
        .await
        .map_err(|e| format!("guild seasons decode failed: {e}"))
}

pub async fn cached_latest_guild_season_leaderboard(
    state: &AppState,
) -> Result<Option<CachedSeasonLeaderboard>, String> {
    {
        let cached = state.season_leaderboard_cache.read().await;
        if let Some(cached) = fresh_season_leaderboard(cached.as_ref(), Utc::now()) {
            return Ok(Some(cached));
        }
    }

    let _refresh_guard = state.season_leaderboard_fetch_lock.lock().await;
    {
        let cached = state.season_leaderboard_cache.read().await;
        if let Some(cached) = fresh_season_leaderboard(cached.as_ref(), Utc::now()) {
            return Ok(Some(cached));
        }
    }

    let Some(season_id) = latest_guild_season_leaderboard_id(&state.http_client).await? else {
        return Ok(None);
    };
    let leaderboard = fetch_guild_season_leaderboard(&state.http_client, season_id, 1000).await?;
    let mut cached = state.season_leaderboard_cache.write().await;
    *cached = Some(leaderboard.clone());
    Ok(Some(leaderboard))
}

pub async fn cached_map_intel_summary(state: &AppState) -> Result<MapIntelSummary, String> {
    {
        let cached = state.map_intel_cache.read().await;
        if let Some(summary) = fresh_map_intel_summary(cached.as_ref(), Utc::now()) {
            return Ok(summary);
        }
    }

    let _refresh_guard = state.map_intel_fetch_lock.lock().await;
    {
        let cached = state.map_intel_cache.read().await;
        if let Some(summary) = fresh_map_intel_summary(cached.as_ref(), Utc::now()) {
            return Ok(summary);
        }
    }

    let payload = fetch_map_intel_payload(&state.http_client).await?;
    let summary = payload.summary.clone();
    let mut cached = state.map_intel_cache.write().await;
    *cached = Some(CachedMapIntel {
        summary: payload.summary,
        overlay: payload.overlay,
        fetched_at: Utc::now(),
    });
    Ok(summary)
}

pub async fn cached_map_intel_overlay(state: &AppState) -> Result<MapIntelOverlay, String> {
    {
        let cached = state.map_intel_cache.read().await;
        if let Some(overlay) = fresh_map_intel_overlay(cached.as_ref(), Utc::now()) {
            return Ok(overlay);
        }
    }

    let _refresh_guard = state.map_intel_fetch_lock.lock().await;
    {
        let cached = state.map_intel_cache.read().await;
        if let Some(overlay) = fresh_map_intel_overlay(cached.as_ref(), Utc::now()) {
            return Ok(overlay);
        }
    }

    let payload = fetch_map_intel_payload(&state.http_client).await?;
    let overlay = payload.overlay.clone();
    let mut cached = state.map_intel_cache.write().await;
    *cached = Some(CachedMapIntel {
        summary: payload.summary,
        overlay: payload.overlay,
        fetched_at: Utc::now(),
    });
    Ok(overlay)
}

fn fresh_season_leaderboard(
    cached: Option<&CachedSeasonLeaderboard>,
    now: DateTime<Utc>,
) -> Option<CachedSeasonLeaderboard> {
    let cached = cached?;
    let age = now.signed_duration_since(cached.fetched_at).num_seconds();
    (age < SEASON_LEADERBOARD_CACHE_TTL_SECS).then(|| cached.clone())
}

fn fresh_map_intel_summary(
    cached: Option<&CachedMapIntel>,
    now: DateTime<Utc>,
) -> Option<MapIntelSummary> {
    let cached = cached?;
    let age = now.signed_duration_since(cached.fetched_at).num_seconds();
    (age < MAP_INTEL_CACHE_TTL_SECS).then(|| cached.summary.clone())
}

fn fresh_map_intel_overlay(
    cached: Option<&CachedMapIntel>,
    now: DateTime<Utc>,
) -> Option<MapIntelOverlay> {
    let cached = cached?;
    let age = now.signed_duration_since(cached.fetched_at).num_seconds();
    (age < MAP_INTEL_CACHE_TTL_SECS).then(|| cached.overlay.clone())
}

async fn fetch_map_intel_payload(client: &reqwest::Client) -> Result<MapIntelPayload, String> {
    let (raids, camps, world_events, gathering_nodes) = tokio::try_join!(
        fetch_json_vec::<RawMapActivity>(client, WYNNCRAFT_MAP_RAIDS_URL, "map raids"),
        fetch_json_vec::<RawMapActivity>(client, WYNNCRAFT_MAP_CAMPS_URL, "map camps"),
        fetch_json_vec::<RawWorldEvent>(client, WYNNCRAFT_MAP_WORLD_EVENTS_URL, "world events"),
        fetch_json_vec::<RawGatheringNode>(
            client,
            WYNNCRAFT_MAP_GATHERING_NODES_URL,
            "gathering nodes"
        ),
    )?;

    let generated_at = Utc::now().to_rfc3339();
    let source = "wynncraft_api".to_string();
    let raids_summary = summarize_activities(&raids);
    let camps_summary = summarize_activities(&camps);
    let world_events_summary = summarize_world_events(&world_events);
    let gathering_nodes_summary = summarize_gathering_nodes(&gathering_nodes);

    Ok(MapIntelPayload {
        summary: MapIntelSummary {
            generated_at: generated_at.clone(),
            source: source.clone(),
            raids: raids_summary,
            camps: camps_summary,
            world_events: world_events_summary,
            gathering_nodes: gathering_nodes_summary.clone(),
        },
        overlay: MapIntelOverlay {
            generated_at,
            source,
            raids: activity_markers(&raids),
            camps: activity_markers(&camps),
            world_events: world_event_markers(&world_events),
            gathering_nodes: gathering_node_markers(&gathering_nodes),
            gathering_resources: gathering_nodes_summary.resources,
            gathering_node_types: gathering_nodes_summary.node_types,
        },
    })
}

async fn fetch_json_vec<T>(
    client: &reqwest::Client,
    url: &str,
    label: &str,
) -> Result<Vec<T>, String>
where
    T: DeserializeOwned,
{
    client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{label} request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("{label} status: {e}"))?
        .json::<Vec<T>>()
        .await
        .map_err(|e| format!("{label} decode failed: {e}"))
}

fn summarize_activities(entries: &[RawMapActivity]) -> MapActivityCollectionSummary {
    let mut difficulties = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    let mut min_level = None;
    let mut max_level = None;
    let mut summaries = Vec::with_capacity(entries.len());

    for entry in entries {
        count_label(&mut difficulties, entry.difficulty.as_deref());
        count_label(&mut lengths, entry.length.as_deref());
        update_level_bounds(&mut min_level, &mut max_level, entry.level);

        summaries.push(MapActivitySummary {
            name: entry.name.clone(),
            internal_name: entry.internal_name.clone(),
            kind: entry.kind.clone(),
            difficulty: clean_optional_label(entry.difficulty.clone()),
            level: entry.level,
            length: clean_optional_label(entry.length.clone()),
            location: entry.location,
            requirement_count: entry.requirements.as_ref().map_or(0, Vec::len),
            rewards: summarize_rewards(&entry.rewards),
        });
    }

    summaries.sort_by(|left, right| {
        left.level
            .unwrap_or(i32::MAX)
            .cmp(&right.level.unwrap_or(i32::MAX))
            .then_with(|| left.name.cmp(&right.name))
    });

    MapActivityCollectionSummary {
        count: summaries.len(),
        min_level,
        max_level,
        difficulties: sorted_counts(difficulties),
        lengths: sorted_counts(lengths),
        entries: summaries,
    }
}

fn activity_markers(entries: &[RawMapActivity]) -> Vec<MapActivityMarker> {
    let mut markers = entries
        .iter()
        .filter_map(|entry| {
            Some(MapActivityMarker {
                name: entry.name.clone(),
                internal_name: entry.internal_name.clone(),
                kind: entry.kind.clone(),
                difficulty: clean_optional_label(entry.difficulty.clone()),
                level: entry.level,
                length: clean_optional_label(entry.length.clone()),
                location: entry.location?,
            })
        })
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        left.level
            .unwrap_or(i32::MAX)
            .cmp(&right.level.unwrap_or(i32::MAX))
            .then_with(|| left.name.cmp(&right.name))
    });
    markers
}

fn summarize_rewards(rewards: &[RawMapReward]) -> MapRewardSummary {
    let mut summary = MapRewardSummary {
        total: rewards.len(),
        ..MapRewardSummary::default()
    };
    for reward in rewards {
        if reward.always.unwrap_or(false) {
            summary.always += 1;
        }
        match reward
            .tier
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("MYTHIC") => summary.mythic += 1,
            Some("FABLED") => summary.fabled += 1,
            Some("LEGENDARY") => summary.legendary += 1,
            Some("RARE") => summary.rare += 1,
            Some("UNIQUE") => summary.unique += 1,
            _ => {}
        }
    }
    summary
}

fn summarize_world_events(entries: &[RawWorldEvent]) -> WorldEventCollectionSummary {
    let mut difficulties = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    let mut min_level = None;
    let mut max_level = None;
    let mut scheduled = Vec::new();
    let mut next_schedule = None::<String>;
    let mut next_schedule_at = None::<DateTime<Utc>>;
    let mut fallback_next_schedule = None::<String>;

    for entry in entries.iter() {
        count_label(&mut difficulties, entry.difficulty.as_deref());
        count_label(&mut lengths, entry.length.as_deref());
        update_level_bounds(&mut min_level, &mut max_level, entry.level);

        let Some(schedule) = clean_optional_label(entry.schedule.clone()) else {
            continue;
        };
        let schedule_at = parse_schedule_utc(&schedule);
        if let Some(parsed) = schedule_at {
            if next_schedule_at.is_none_or(|current| parsed < current) {
                next_schedule = Some(schedule.clone());
                next_schedule_at = Some(parsed);
            }
        } else if fallback_next_schedule
            .as_ref()
            .is_none_or(|current| schedule < *current)
        {
            fallback_next_schedule = Some(schedule.clone());
        }
        scheduled.push((
            WorldEventSummary {
                name: entry.name.clone(),
                internal_name: entry.internal_name.clone(),
                difficulty: clean_optional_label(entry.difficulty.clone()),
                level: entry.level,
                length: clean_optional_label(entry.length.clone()),
                schedule: Some(schedule),
                location_count: entry.location.len(),
                first_location: entry.location.iter().find_map(|location| location.event),
            },
            schedule_at,
        ));
    }

    scheduled.sort_by(|left, right| {
        compare_schedule_times(left.1, right.1)
            .then_with(|| left.0.schedule.cmp(&right.0.schedule))
            .then_with(|| {
                left.0
                    .level
                    .unwrap_or(i32::MAX)
                    .cmp(&right.0.level.unwrap_or(i32::MAX))
            })
            .then_with(|| left.0.name.cmp(&right.0.name))
    });
    let scheduled = scheduled
        .into_iter()
        .map(|(summary, _)| summary)
        .collect::<Vec<_>>();

    WorldEventCollectionSummary {
        count: entries.len(),
        scheduled_count: scheduled.len(),
        next_schedule: next_schedule.or(fallback_next_schedule),
        min_level,
        max_level,
        difficulties: sorted_counts(difficulties),
        lengths: sorted_counts(lengths),
        scheduled,
    }
}

fn world_event_markers(entries: &[RawWorldEvent]) -> Vec<WorldEventMarker> {
    let mut markers = entries
        .iter()
        .filter_map(|entry| {
            let locations = entry
                .location
                .iter()
                .filter_map(|location| location.event)
                .collect::<Vec<_>>();
            (!locations.is_empty()).then(|| WorldEventMarker {
                name: entry.name.clone(),
                internal_name: entry.internal_name.clone(),
                difficulty: clean_optional_label(entry.difficulty.clone()),
                level: entry.level,
                length: clean_optional_label(entry.length.clone()),
                schedule: clean_optional_label(entry.schedule.clone()),
                locations,
            })
        })
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        compare_schedule_times(
            left.schedule.as_deref().and_then(parse_schedule_utc),
            right.schedule.as_deref().and_then(parse_schedule_utc),
        )
        .then_with(|| {
            left.level
                .unwrap_or(i32::MAX)
                .cmp(&right.level.unwrap_or(i32::MAX))
        })
        .then_with(|| left.name.cmp(&right.name))
    });
    markers
}

fn parse_schedule_utc(schedule: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(schedule)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn compare_schedule_times(left: Option<DateTime<Utc>>, right: Option<DateTime<Utc>>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn summarize_gathering_nodes(entries: &[RawGatheringNode]) -> GatheringNodeCollectionSummary {
    let mut resources = BTreeMap::new();
    let mut node_types = BTreeMap::new();
    let mut min_level = None;
    let mut max_level = None;

    for entry in entries.iter() {
        count_label(&mut resources, Some(entry.resource.as_str()));
        count_label(&mut node_types, Some(entry.node_type.as_str()));
        update_level_bounds(&mut min_level, &mut max_level, entry.level);
    }

    GatheringNodeCollectionSummary {
        count: entries.len(),
        min_level,
        max_level,
        resources: sorted_counts(resources),
        node_types: sorted_counts(node_types),
    }
}

fn gathering_node_markers(entries: &[RawGatheringNode]) -> Vec<GatheringNodeMarker> {
    entries
        .iter()
        .filter_map(|entry| {
            Some(GatheringNodeMarker {
                location: MapPoint {
                    x: entry.x?,
                    y: entry.y?,
                    z: entry.z?,
                },
                node_type: clean_label(entry.node_type.as_str()),
                resource: clean_label(entry.resource.as_str()),
                level: entry.level,
                angle: entry.angle,
            })
        })
        .collect()
}

fn update_level_bounds(
    min_level: &mut Option<i32>,
    max_level: &mut Option<i32>,
    level: Option<i32>,
) {
    let Some(level) = level else {
        return;
    };
    *min_level = Some(min_level.map_or(level, |current| current.min(level)));
    *max_level = Some(max_level.map_or(level, |current| current.max(level)));
}

fn count_label(counts: &mut BTreeMap<String, usize>, label: Option<&str>) {
    let Some(label) = label.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    *counts.entry(label.replace('_', " ")).or_default() += 1;
}

fn clean_optional_label(label: Option<String>) -> Option<String> {
    label
        .map(|value| clean_label(value.as_str()))
        .filter(|value| !value.is_empty())
}

fn clean_label(label: &str) -> String {
    label.trim().replace('_', " ")
}

fn sorted_counts(counts: BTreeMap<String, usize>) -> Vec<NamedCount> {
    let mut counts = counts
        .into_iter()
        .map(|(name, count)| NamedCount { name, count })
        .collect::<Vec<_>>();
    counts.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    counts
}

async fn latest_guild_season_leaderboard_id(
    client: &reqwest::Client,
) -> Result<Option<i32>, String> {
    let types = client
        .get(WYNNCRAFT_LEADERBOARD_TYPES_URL)
        .send()
        .await
        .map_err(|e| format!("leaderboard types request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("leaderboard types status: {e}"))?
        .json::<Vec<String>>()
        .await
        .map_err(|e| format!("leaderboard types decode failed: {e}"))?;

    Ok(types
        .into_iter()
        .filter_map(|name| name.strip_prefix("guildSeason")?.parse::<i32>().ok())
        .max())
}

async fn fetch_guild_season_leaderboard(
    client: &reqwest::Client,
    season_id: i32,
    result_limit: u16,
) -> Result<CachedSeasonLeaderboard, String> {
    let url = format!("{WYNNCRAFT_LEADERBOARDS_URL}/guildSeason{season_id}");
    let raw_entries = client
        .get(url)
        .query(&[("resultLimit", result_limit)])
        .send()
        .await
        .map_err(|e| format!("season leaderboard request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("season leaderboard status: {e}"))?
        .json::<HashMap<String, RawLeaderboardEntry>>()
        .await
        .map_err(|e| format!("season leaderboard decode failed: {e}"))?;

    let mut entries = raw_entries
        .into_iter()
        .filter_map(|(rank, entry)| {
            let rank = rank.parse::<u32>().ok()?;
            if entry.name.trim().is_empty() {
                return None;
            }
            Some(CachedSeasonLeaderboardEntry {
                rank,
                name: entry.name,
                uuid: entry.uuid,
                prefix: entry.prefix,
                score: entry.score,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.rank);

    Ok(CachedSeasonLeaderboard {
        season_id,
        entries,
        fetched_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(name: &str, level: i32, rewards: Vec<RawMapReward>) -> RawMapActivity {
        RawMapActivity {
            name: name.to_string(),
            internal_name: name.to_ascii_lowercase().replace(' ', "_"),
            kind: "RAID".to_string(),
            difficulty: Some("HARD".to_string()),
            level: Some(level),
            length: Some("MEDIUM".to_string()),
            requirements: Some(vec![
                serde_json::json!({"type": "COMBAT_LEVEL", "value": level}),
            ]),
            location: Some(MapPoint {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            }),
            rewards,
        }
    }

    #[test]
    fn activity_summary_counts_levels_and_reward_tiers() {
        let entries = vec![
            activity(
                "Beta Raid",
                80,
                vec![
                    RawMapReward {
                        always: Some(true),
                        tier: Some("MYTHIC".to_string()),
                    },
                    RawMapReward {
                        always: Some(false),
                        tier: Some("FABLED".to_string()),
                    },
                ],
            ),
            activity(
                "Alpha Raid",
                60,
                vec![RawMapReward {
                    always: None,
                    tier: Some("LEGENDARY".to_string()),
                }],
            ),
        ];
        let summary = summarize_activities(&entries);

        assert_eq!(summary.count, 2);
        assert_eq!(summary.min_level, Some(60));
        assert_eq!(summary.max_level, Some(80));
        assert_eq!(summary.difficulties[0].name, "HARD");
        assert_eq!(summary.difficulties[0].count, 2);
        assert_eq!(summary.entries[0].name, "Alpha Raid");
        assert_eq!(summary.entries[1].rewards.total, 2);
        assert_eq!(summary.entries[1].rewards.always, 1);
        assert_eq!(summary.entries[1].rewards.mythic, 1);
        assert_eq!(summary.entries[1].rewards.fabled, 1);
    }

    #[test]
    fn world_event_summary_orders_visible_schedules() {
        let entries = vec![
            RawWorldEvent {
                name: "Later".to_string(),
                internal_name: "later".to_string(),
                difficulty: Some("EASY".to_string()),
                level: Some(10),
                length: Some("SHORT".to_string()),
                location: vec![RawWorldEventLocation {
                    event: Some(MapPoint {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0,
                    }),
                }],
                schedule: Some("2026-05-25T11:00:00Z".to_string()),
            },
            RawWorldEvent {
                name: "Hidden".to_string(),
                internal_name: "hidden".to_string(),
                difficulty: None,
                level: None,
                length: None,
                location: Vec::new(),
                schedule: None,
            },
            RawWorldEvent {
                name: "Sooner".to_string(),
                internal_name: "sooner".to_string(),
                difficulty: Some("HARD".to_string()),
                level: Some(30),
                length: Some("MEDIUM".to_string()),
                location: Vec::new(),
                schedule: Some("2026-05-25T12:30:00+02:00".to_string()),
            },
        ];
        let summary = summarize_world_events(&entries);

        assert_eq!(summary.count, 3);
        assert_eq!(summary.scheduled_count, 2);
        assert_eq!(
            summary.next_schedule.as_deref(),
            Some("2026-05-25T12:30:00+02:00")
        );
        assert_eq!(summary.scheduled[0].name, "Sooner");
        assert_eq!(summary.scheduled[1].name, "Later");
    }

    #[test]
    fn gathering_summary_counts_resources_and_node_types() {
        let entries = vec![
            RawGatheringNode {
                x: Some(1.0),
                y: Some(2.0),
                z: Some(3.0),
                angle: Some(0.0),
                node_type: "NODE".to_string(),
                resource: "COPPER".to_string(),
                level: Some(1),
            },
            RawGatheringNode {
                x: Some(4.0),
                y: Some(5.0),
                z: Some(6.0),
                angle: Some(90.0),
                node_type: "WALL".to_string(),
                resource: "COPPER".to_string(),
                level: Some(5),
            },
            RawGatheringNode {
                x: Some(7.0),
                y: Some(8.0),
                z: Some(9.0),
                angle: None,
                node_type: "NODE".to_string(),
                resource: "OAK".to_string(),
                level: Some(3),
            },
        ];
        let summary = summarize_gathering_nodes(&entries);

        assert_eq!(summary.count, 3);
        assert_eq!(summary.min_level, Some(1));
        assert_eq!(summary.max_level, Some(5));
        assert_eq!(summary.resources[0].name, "COPPER");
        assert_eq!(summary.resources[0].count, 2);
        assert_eq!(summary.node_types[0].name, "NODE");
        assert_eq!(summary.node_types[0].count, 2);
    }

    #[test]
    fn gathering_node_markers_include_coordinates() {
        let markers = gathering_node_markers(&[
            RawGatheringNode {
                x: Some(-1751.0),
                y: Some(59.0),
                z: Some(-4420.0),
                angle: Some(180.0),
                node_type: "CORNER".to_string(),
                resource: "COPPER".to_string(),
                level: Some(1),
            },
            RawGatheringNode {
                x: None,
                y: Some(59.0),
                z: Some(-4420.0),
                angle: None,
                node_type: "NODE".to_string(),
                resource: "OAK".to_string(),
                level: Some(1),
            },
        ]);

        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].location.x, -1751.0);
        assert_eq!(markers[0].location.z, -4420.0);
        assert_eq!(markers[0].resource, "COPPER");
    }
}
