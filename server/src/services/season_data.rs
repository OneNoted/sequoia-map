use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config;
use crate::services::season_components::{self, SeasonComponentPoint};
use crate::services::wynncraft_api;
use crate::state::AppState;

type SeasonMetadataRow = (i32, Option<String>, DateTime<Utc>, DateTime<Utc>, String);
type InferredSeasonRow = (i32, DateTime<Utc>, DateTime<Utc>);
type LatestGuildRow = (String, String, i32, DateTime<Utc>);
type SeriesRow = (String, DateTime<Utc>, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeasonDataError {
    Unavailable,
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeasonWindowSource {
    Configured,
    WynncraftApi,
    Inferred,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SeasonWindow {
    pub season_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub source: SeasonWindowSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub territory_holding_sr_per_hour: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sr_per_war: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonWindowsResponse {
    pub seasons: Vec<SeasonWindow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonSeriesPoint {
    pub sampled_at: String,
    pub season_rating: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonSeriesEntry {
    pub guild_name: String,
    pub guild_prefix: String,
    pub current_sr: i64,
    pub current_rank: u32,
    pub sample_count: u32,
    pub last_sampled_at: String,
    pub series: Vec<SeasonSeriesPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raid_sr_series: Vec<SeasonComponentPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passive_hold_sr_series: Vec<SeasonComponentPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conquest_sr_series: Vec<SeasonComponentPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub daily_raid_count_series: Vec<SeasonComponentPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonSeriesResponse {
    pub season_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub source: SeasonWindowSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub territory_holding_sr_per_hour: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sr_per_war: Option<i32>,
    pub generated_at: String,
    pub entries: Vec<SeasonSeriesEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSeasonWindow {
    pub season_id: i32,
    pub label: Option<String>,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub source: SeasonWindowSource,
    pub territory_holding_sr_per_hour: Option<i32>,
    pub sr_per_war: Option<i32>,
}

#[derive(Debug, Clone)]
struct SeriesObservation {
    observed_at: DateTime<Utc>,
    season_rating: i64,
}

pub async fn build_windows_response(
    state: &AppState,
) -> Result<SeasonWindowsResponse, SeasonDataError> {
    let seasons = list_resolved_windows(state)
        .await?
        .into_iter()
        .map(|window| window.into_response())
        .collect();
    Ok(SeasonWindowsResponse { seasons })
}

pub async fn build_series_response(
    state: &AppState,
    requested_season_id: Option<i32>,
    guild_names: &[String],
) -> Result<SeasonSeriesResponse, SeasonDataError> {
    let Some(pool) = state.db.as_ref() else {
        return Err(SeasonDataError::Unavailable);
    };

    let requested_names = normalize_requested_guild_names(guild_names);
    if requested_names.is_empty() {
        return Err(SeasonDataError::BadRequest);
    }

    let window = resolve_requested_window(state, requested_season_id)
        .await?
        .ok_or(SeasonDataError::Unavailable)?;
    let generated_at = Utc::now();
    let range_end = generated_at.min(window.end_at);
    let requested_lookup = build_requested_lookup(&requested_names);

    let latest_rows: Vec<LatestGuildRow> = sqlx::query_as(
        "SELECT guild_name, COALESCE(guild_prefix, ''), season_rating, observed_at \
         FROM ( \
             SELECT DISTINCT ON (LOWER(guild_name)) guild_name, guild_prefix, season_rating, observed_at \
             FROM season_guild_observations \
             WHERE season_id = $1 \
               AND LOWER(guild_name) = ANY($2) \
               AND observed_at >= $3 \
               AND observed_at <= $4 \
             ORDER BY LOWER(guild_name), observed_at DESC \
         ) latest \
         ORDER BY season_rating DESC, guild_name ASC",
    )
    .bind(window.season_id)
    .bind(&requested_names)
    .bind(window.start_at)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonDataError::Internal)?;

    let names_for_series: Vec<String> = latest_rows
        .iter()
        .map(|row| row.0.to_ascii_lowercase())
        .collect();
    let chart_rows: Vec<SeriesRow> = sqlx::query_as(
        "SELECT guild_name, observed_at, season_rating \
         FROM ( \
             SELECT DISTINCT ON (LOWER(guild_name), date_trunc('hour', observed_at)) \
                 guild_name, observed_at, season_rating \
             FROM season_guild_observations \
             WHERE season_id = $1 \
               AND LOWER(guild_name) = ANY($2) \
               AND observed_at >= $3 \
               AND observed_at <= $4 \
             ORDER BY LOWER(guild_name), date_trunc('hour', observed_at), observed_at DESC \
         ) hourly \
         ORDER BY LOWER(guild_name) ASC, observed_at ASC",
    )
    .bind(window.season_id)
    .bind(&names_for_series)
    .bind(window.start_at)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonDataError::Internal)?;

    let mut series_by_name: HashMap<String, Vec<SeriesObservation>> = HashMap::new();
    for (guild_name, observed_at, season_rating) in chart_rows {
        series_by_name
            .entry(guild_name.to_ascii_lowercase())
            .or_default()
            .push(SeriesObservation {
                observed_at,
                season_rating: i64::from(season_rating),
            });
    }

    let total_guilds = u32::try_from(latest_rows.len()).unwrap_or(u32::MAX);
    let mut entries = Vec::with_capacity(latest_rows.len());
    let actual_guild_names = latest_rows
        .iter()
        .map(|row| row.0.clone())
        .collect::<Vec<_>>();
    let components_by_name =
        season_components::build_components(state, &window, &actual_guild_names, range_end)
            .await
            .map_err(|_| SeasonDataError::Internal)?;
    for (idx, (guild_name, guild_prefix, season_rating, observed_at)) in
        latest_rows.into_iter().enumerate()
    {
        let normalized_name = guild_name.to_ascii_lowercase();
        let observations = series_by_name.remove(&normalized_name).unwrap_or_default();
        let components = components_by_name.get(&normalized_name);
        entries.push((
            requested_lookup
                .get(&normalized_name)
                .copied()
                .unwrap_or(usize::MAX),
            idx,
            SeasonSeriesEntry {
                guild_name,
                guild_prefix,
                current_sr: i64::from(season_rating),
                current_rank: rank_from_sorted_index(idx, total_guilds),
                sample_count: u32::try_from(observations.len()).unwrap_or(u32::MAX),
                last_sampled_at: observed_at.to_rfc3339(),
                series: downsample_series_hourly(observations),
                raid_sr_series: components
                    .map(|components| components.cumulative_raid_sr_series.clone())
                    .unwrap_or_default(),
                passive_hold_sr_series: components
                    .map(|components| components.cumulative_passive_hold_sr_series.clone())
                    .unwrap_or_default(),
                conquest_sr_series: components
                    .map(|components| components.cumulative_conquest_sr_series.clone())
                    .unwrap_or_default(),
                daily_raid_count_series: components
                    .map(|components| components.daily_raid_count_series.clone())
                    .unwrap_or_default(),
            },
        ));
    }

    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let entries = entries.into_iter().map(|(_, _, entry)| entry).collect();

    Ok(SeasonSeriesResponse {
        season_id: window.season_id,
        label: window.label.clone(),
        start_at: window.start_at.to_rfc3339(),
        end_at: window.end_at.to_rfc3339(),
        source: window.source,
        territory_holding_sr_per_hour: window.territory_holding_sr_per_hour,
        sr_per_war: window.sr_per_war,
        generated_at: generated_at.to_rfc3339(),
        entries,
    })
}

pub async fn resolve_requested_window(
    state: &AppState,
    requested_season_id: Option<i32>,
) -> Result<Option<ResolvedSeasonWindow>, SeasonDataError> {
    let windows = list_resolved_windows(state).await?;
    if windows.is_empty() {
        return Ok(None);
    }

    if let Some(season_id) = requested_season_id {
        let window = windows
            .into_iter()
            .find(|window| window.season_id == season_id);
        if window.is_none() {
            return Err(SeasonDataError::BadRequest);
        }
        return Ok(window);
    }

    if let Some(active) = active_window_from_config()? {
        return Ok(Some(active));
    }

    Ok(windows.into_iter().max_by_key(|window| window.season_id))
}

pub async fn list_resolved_windows(
    state: &AppState,
) -> Result<Vec<ResolvedSeasonWindow>, SeasonDataError> {
    let active = active_window_from_config()?;
    let api_windows = wynncraft_api::fetch_guild_seasons(&state.http_client)
        .await
        .map(api_season_windows)
        .unwrap_or_default();
    let Some(pool) = state.db.as_ref() else {
        return Ok(merge_windows(Vec::new(), Vec::new(), active, api_windows));
    };

    let metadata_rows: Vec<SeasonMetadataRow> = sqlx::query_as(
        "SELECT season_id, NULLIF(TRIM(label), ''), start_at, end_at, source \
         FROM season_metadata \
         ORDER BY season_id DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonDataError::Internal)?;

    let inferred_rows: Vec<InferredSeasonRow> = sqlx::query_as(
        "SELECT season_id, MIN(observed_at), MAX(observed_at) \
         FROM season_guild_observations \
         GROUP BY season_id \
         ORDER BY season_id ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonDataError::Internal)?;

    let windows = merge_windows(metadata_rows, inferred_rows, active, api_windows);
    Ok(windows)
}

fn active_window_from_config() -> Result<Option<ResolvedSeasonWindow>, SeasonDataError> {
    Ok(config::active_season_race_config()
        .map_err(|_| SeasonDataError::Internal)?
        .map(|active| ResolvedSeasonWindow {
            season_id: active.season_id,
            label: active.label,
            start_at: active.start_at,
            end_at: active.end_at,
            source: SeasonWindowSource::Configured,
            territory_holding_sr_per_hour: None,
            sr_per_war: None,
        }))
}

fn merge_windows(
    metadata_rows: Vec<SeasonMetadataRow>,
    inferred_rows: Vec<InferredSeasonRow>,
    active: Option<ResolvedSeasonWindow>,
    api_windows: Vec<ResolvedSeasonWindow>,
) -> Vec<ResolvedSeasonWindow> {
    let mut merged: HashMap<i32, ResolvedSeasonWindow> = HashMap::new();

    for (idx, (season_id, start_at, observed_last_at)) in inferred_rows.iter().enumerate() {
        let inferred_end = inferred_rows
            .get(idx + 1)
            .map(|(_, next_start_at, _)| *next_start_at)
            .unwrap_or(*observed_last_at);
        if inferred_end <= *start_at {
            continue;
        }
        merged.insert(
            *season_id,
            ResolvedSeasonWindow {
                season_id: *season_id,
                label: None,
                start_at: *start_at,
                end_at: inferred_end,
                source: SeasonWindowSource::Inferred,
                territory_holding_sr_per_hour: None,
                sr_per_war: None,
            },
        );
    }

    for window in api_windows {
        merged.insert(window.season_id, window);
    }

    for (season_id, label, start_at, end_at, _source) in metadata_rows {
        if end_at <= start_at {
            continue;
        }
        let mut window = ResolvedSeasonWindow {
            season_id,
            label,
            start_at,
            end_at,
            source: SeasonWindowSource::Configured,
            territory_holding_sr_per_hour: None,
            sr_per_war: None,
        };
        inherit_missing_sr_rates(&mut window, &merged);
        merged.insert(season_id, window);
    }

    if let Some(mut active) = active {
        inherit_missing_sr_rates(&mut active, &merged);
        merged.insert(active.season_id, active);
    }

    let mut windows: Vec<ResolvedSeasonWindow> = merged.into_values().collect();
    windows.sort_by_key(|window| std::cmp::Reverse(window.season_id));
    windows
}

fn inherit_missing_sr_rates(
    window: &mut ResolvedSeasonWindow,
    merged: &HashMap<i32, ResolvedSeasonWindow>,
) {
    let Some(existing) = merged.get(&window.season_id) else {
        return;
    };
    if window.territory_holding_sr_per_hour.is_none() {
        window.territory_holding_sr_per_hour = existing.territory_holding_sr_per_hour;
    }
    if window.sr_per_war.is_none() {
        window.sr_per_war = existing.sr_per_war;
    }
}

fn api_season_windows(
    seasons: HashMap<String, wynncraft_api::GuildSeasonDefinition>,
) -> Vec<ResolvedSeasonWindow> {
    seasons
        .into_iter()
        .filter_map(|(season_id, season)| {
            let season_id = season_id.parse::<i32>().ok()?;
            let start_at = season.start_date?;
            let end_at = season.end_date?;
            if end_at <= start_at {
                return None;
            }
            Some(ResolvedSeasonWindow {
                season_id,
                label: Some(format!("Season {season_id}")),
                start_at,
                end_at,
                source: SeasonWindowSource::WynncraftApi,
                territory_holding_sr_per_hour: season.territory_holding_sr_per_hour,
                sr_per_war: season.sr_per_war,
            })
        })
        .collect()
}

fn normalize_requested_guild_names(guild_names: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for guild_name in guild_names {
        let candidate = guild_name.trim().to_ascii_lowercase();
        if candidate.is_empty() || !seen.insert(candidate.clone()) {
            continue;
        }
        normalized.push(candidate);
    }
    normalized
}

fn build_requested_lookup(requested_names: &[String]) -> HashMap<String, usize> {
    requested_names
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.clone(), idx))
        .collect()
}

fn rank_from_sorted_index(idx: usize, total: u32) -> u32 {
    let rank = u32::try_from(idx + 1).unwrap_or(u32::MAX);
    rank.min(total.max(1))
}

fn downsample_series_hourly(observations: Vec<SeriesObservation>) -> Vec<SeasonSeriesPoint> {
    let mut downsampled = Vec::new();
    let mut last_bucket: Option<i64> = None;
    let mut pending: Option<&SeriesObservation> = None;

    for observation in &observations {
        let bucket = observation.observed_at.timestamp() / 3600;
        match last_bucket {
            Some(current_bucket) if current_bucket != bucket => {
                if let Some(point) = pending.take() {
                    downsampled.push(SeasonSeriesPoint {
                        sampled_at: point.observed_at.to_rfc3339(),
                        season_rating: point.season_rating,
                    });
                }
                last_bucket = Some(bucket);
                pending = Some(observation);
            }
            Some(_) => {
                pending = Some(observation);
            }
            None => {
                last_bucket = Some(bucket);
                pending = Some(observation);
            }
        }
    }

    if let Some(point) = pending {
        downsampled.push(SeasonSeriesPoint {
            sampled_at: point.observed_at.to_rfc3339(),
            season_rating: point.season_rating,
        });
    }

    downsampled
}

impl ResolvedSeasonWindow {
    pub fn into_response(self) -> SeasonWindow {
        SeasonWindow {
            season_id: self.season_id,
            label: self.label,
            start_at: self.start_at.to_rfc3339(),
            end_at: self.end_at.to_rfc3339(),
            source: self.source,
            territory_holding_sr_per_hour: self.territory_holding_sr_per_hour,
            sr_per_war: self.sr_per_war,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ResolvedSeasonWindow, SeasonWindowSource, merge_windows, normalize_requested_guild_names,
    };
    use chrono::{DateTime, Utc};

    fn ts(value: &str) -> DateTime<Utc> {
        value.parse::<DateTime<Utc>>().expect("parse timestamp")
    }

    #[test]
    fn merge_windows_uses_next_season_start_as_inferred_end() {
        let windows = merge_windows(
            Vec::new(),
            vec![
                (29, ts("2026-02-01T00:00:00Z"), ts("2026-02-27T23:00:00Z")),
                (30, ts("2026-03-01T00:00:00Z"), ts("2026-03-26T23:00:00Z")),
            ],
            None,
            Vec::new(),
        );

        assert_eq!(
            windows,
            vec![
                ResolvedSeasonWindow {
                    season_id: 30,
                    label: None,
                    start_at: ts("2026-03-01T00:00:00Z"),
                    end_at: ts("2026-03-26T23:00:00Z"),
                    source: SeasonWindowSource::Inferred,
                    territory_holding_sr_per_hour: None,
                    sr_per_war: None,
                },
                ResolvedSeasonWindow {
                    season_id: 29,
                    label: None,
                    start_at: ts("2026-02-01T00:00:00Z"),
                    end_at: ts("2026-03-01T00:00:00Z"),
                    source: SeasonWindowSource::Inferred,
                    territory_holding_sr_per_hour: None,
                    sr_per_war: None,
                },
            ]
        );
    }

    #[test]
    fn merge_windows_prefers_configured_window_over_inferred() {
        let windows = merge_windows(
            vec![(
                30,
                Some("Season 30".to_string()),
                ts("2026-03-26T03:53:21Z"),
                ts("2026-04-22T06:49:05Z"),
                "configured".to_string(),
            )],
            vec![(30, ts("2026-03-26T00:00:00Z"), ts("2026-03-27T00:00:00Z"))],
            None,
            Vec::new(),
        );

        assert_eq!(
            windows,
            vec![ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Season 30".to_string()),
                start_at: ts("2026-03-26T03:53:21Z"),
                end_at: ts("2026-04-22T06:49:05Z"),
                source: SeasonWindowSource::Configured,
                territory_holding_sr_per_hour: None,
                sr_per_war: None,
            }]
        );
    }

    #[test]
    fn merge_windows_prefers_api_window_over_inferred() {
        let api_window = ResolvedSeasonWindow {
            season_id: 30,
            label: Some("Season 30".to_string()),
            start_at: ts("2026-03-26T03:53:21Z"),
            end_at: ts("2026-04-22T06:49:05Z"),
            source: SeasonWindowSource::WynncraftApi,
            territory_holding_sr_per_hour: Some(120),
            sr_per_war: Some(380),
        };
        let windows = merge_windows(
            Vec::new(),
            vec![(30, ts("2026-03-26T00:00:00Z"), ts("2026-03-27T00:00:00Z"))],
            None,
            vec![api_window.clone()],
        );

        assert_eq!(windows, vec![api_window]);
    }

    #[test]
    fn merge_windows_preserves_api_rates_when_metadata_overlays_window() {
        let windows = merge_windows(
            vec![(
                30,
                Some("Season Thirty".to_string()),
                ts("2026-03-27T00:00:00Z"),
                ts("2026-04-23T00:00:00Z"),
                "configured".to_string(),
            )],
            Vec::new(),
            None,
            vec![ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Season 30".to_string()),
                start_at: ts("2026-03-26T03:53:21Z"),
                end_at: ts("2026-04-22T06:49:05Z"),
                source: SeasonWindowSource::WynncraftApi,
                territory_holding_sr_per_hour: Some(120),
                sr_per_war: Some(380),
            }],
        );

        assert_eq!(
            windows,
            vec![ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Season Thirty".to_string()),
                start_at: ts("2026-03-27T00:00:00Z"),
                end_at: ts("2026-04-23T00:00:00Z"),
                source: SeasonWindowSource::Configured,
                territory_holding_sr_per_hour: Some(120),
                sr_per_war: Some(380),
            }]
        );
    }

    #[test]
    fn merge_windows_preserves_api_rates_when_active_window_overlays_api() {
        let windows = merge_windows(
            Vec::new(),
            Vec::new(),
            Some(ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Active Season".to_string()),
                start_at: ts("2026-03-27T00:00:00Z"),
                end_at: ts("2026-04-23T00:00:00Z"),
                source: SeasonWindowSource::Configured,
                territory_holding_sr_per_hour: None,
                sr_per_war: None,
            }),
            vec![ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Season 30".to_string()),
                start_at: ts("2026-03-26T03:53:21Z"),
                end_at: ts("2026-04-22T06:49:05Z"),
                source: SeasonWindowSource::WynncraftApi,
                territory_holding_sr_per_hour: Some(120),
                sr_per_war: Some(380),
            }],
        );

        assert_eq!(
            windows,
            vec![ResolvedSeasonWindow {
                season_id: 30,
                label: Some("Active Season".to_string()),
                start_at: ts("2026-03-27T00:00:00Z"),
                end_at: ts("2026-04-23T00:00:00Z"),
                source: SeasonWindowSource::Configured,
                territory_holding_sr_per_hour: Some(120),
                sr_per_war: Some(380),
            }]
        );
    }

    #[test]
    fn normalize_requested_guild_names_deduplicates_and_trims() {
        assert_eq!(
            normalize_requested_guild_names(&[
                " Sequoia ".to_string(),
                "sequoia".to_string(),
                "Aequitas".to_string(),
                "".to_string(),
            ]),
            vec!["sequoia".to_string(), "aequitas".to_string()]
        );
    }
}
