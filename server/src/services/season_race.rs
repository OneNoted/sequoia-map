use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use sequoia_shared::{SeasonScalarSample, passive_sr_per_hour};

use crate::config;
use crate::state::AppState;

type LatestGuildRow = (String, String, String, i16, i32, DateTime<Utc>);
type SeriesRow = (String, DateTime<Utc>, i32, i16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeasonRaceError {
    Unavailable,
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastSource {
    ObservedTrend,
    PassiveFallback,
    FlatFallback,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRacePoint {
    pub sampled_at: String,
    pub season_rating: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRaceEntry {
    pub guild_name: String,
    pub guild_prefix: String,
    pub current_sr: i64,
    pub projected_final_sr: i64,
    pub current_rank: u32,
    pub projected_rank: u32,
    pub territory_count: usize,
    pub sample_count: u32,
    pub last_sampled_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_rate_per_hour: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_rate_per_hour: Option<f64>,
    pub forecast_rate_per_hour: f64,
    pub forecast_source: ForecastSource,
    pub series: Vec<SeasonRacePoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRaceAssumptions {
    pub lookback_hours: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_scalar_weighted: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRaceResponse {
    pub season_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub generated_at: String,
    pub remaining_hours: f64,
    pub entries: Vec<SeasonRaceEntry>,
    pub assumptions: SeasonRaceAssumptions,
}

#[derive(Debug, Clone)]
struct SeriesObservation {
    observed_at: DateTime<Utc>,
    season_rating: i64,
}

pub async fn build_race_response(
    state: &AppState,
    requested_season_id: Option<i32>,
) -> Result<SeasonRaceResponse, SeasonRaceError> {
    let Some(pool) = state.db.as_ref() else {
        return Err(SeasonRaceError::Unavailable);
    };
    let active = config::active_season_race_config()
        .map_err(|_| SeasonRaceError::Internal)?
        .ok_or(SeasonRaceError::Unavailable)?;
    if requested_season_id.is_some_and(|season_id| season_id != active.season_id) {
        return Err(SeasonRaceError::BadRequest);
    }

    let generated_at = Utc::now();
    let range_end = generated_at.min(active.end_at);
    let remaining_hours = ((active.end_at - generated_at).num_seconds().max(0) as f64) / 3600.0;

    let latest_rows: Vec<LatestGuildRow> = sqlx::query_as(
        "SELECT guild_name, COALESCE(guild_uuid, ''), COALESCE(guild_prefix, ''), territory_count, season_rating, observed_at \
         FROM ( \
             SELECT DISTINCT ON (guild_name) guild_name, guild_uuid, guild_prefix, territory_count, season_rating, observed_at \
             FROM season_guild_observations \
             WHERE season_id = $1 \
               AND observed_at >= $2 \
               AND observed_at <= $3 \
             ORDER BY guild_name, observed_at DESC \
         ) latest \
         ORDER BY season_rating DESC, territory_count DESC, guild_name ASC \
         LIMIT $4",
    )
    .bind(active.season_id)
    .bind(active.start_at)
    .bind(range_end)
    .bind(i64::try_from(active.top_guilds).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonRaceError::Internal)?;

    if latest_rows.is_empty() {
        return Ok(SeasonRaceResponse {
            season_id: active.season_id,
            label: active.label,
            start_at: active.start_at.to_rfc3339(),
            end_at: active.end_at.to_rfc3339(),
            generated_at: generated_at.to_rfc3339(),
            remaining_hours,
            entries: Vec::new(),
            assumptions: SeasonRaceAssumptions {
                lookback_hours: active.lookback_hours,
                passive_scalar_weighted: latest_scalar_weighted_for_season(state, active.season_id)
                    .await,
                note: "Projection assumes the current pace continues from recent observed season rating snapshots."
                    .to_string(),
            },
        });
    }

    let guild_names: Vec<String> = latest_rows.iter().map(|row| row.0.clone()).collect();
    let series_rows: Vec<SeriesRow> = sqlx::query_as(
        "SELECT guild_name, observed_at, season_rating, territory_count \
         FROM season_guild_observations \
         WHERE season_id = $1 \
           AND guild_name = ANY($2) \
           AND observed_at >= $3 \
           AND observed_at <= $4 \
         ORDER BY guild_name ASC, observed_at ASC",
    )
    .bind(active.season_id)
    .bind(&guild_names)
    .bind(active.start_at)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonRaceError::Internal)?;

    let mut series_by_guild: HashMap<String, Vec<SeriesObservation>> = HashMap::new();
    let mut latest_observed_territory_count: HashMap<String, usize> = HashMap::new();
    for (guild_name, observed_at, season_rating, territory_count) in series_rows {
        series_by_guild
            .entry(guild_name.clone())
            .or_default()
            .push(SeriesObservation {
                observed_at,
                season_rating: i64::from(season_rating),
            });
        latest_observed_territory_count.insert(
            guild_name,
            usize::try_from(territory_count.max(0)).unwrap_or(0),
        );
    }

    let live_territory_counts = live_territory_counts(state).await;
    let passive_scalar = latest_scalar_sample_for_season(state, active.season_id)
        .await
        .map(|sample| sample.scalar_weighted);

    let mut entries = Vec::with_capacity(latest_rows.len());
    for (
        idx,
        (guild_name, _guild_uuid, guild_prefix, territory_count, season_rating, observed_at),
    ) in latest_rows.into_iter().enumerate()
    {
        let current_rank = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let current_sr = i64::from(season_rating);
        let live_count = live_territory_counts.get(&guild_name).copied().unwrap_or(0);
        let fallback_count = latest_observed_territory_count
            .get(&guild_name)
            .copied()
            .unwrap_or_else(|| usize::try_from(territory_count.max(0)).unwrap_or(0));
        let current_territory_count = live_count.max(fallback_count);
        let observations = series_by_guild.remove(&guild_name).unwrap_or_default();
        let observed_rate = observed_rate_per_hour(
            &observations,
            active.start_at,
            range_end,
            active.lookback_hours,
        );
        let passive_rate =
            passive_scalar.map(|scalar| passive_sr_per_hour(current_territory_count, scalar));
        let (forecast_rate, forecast_source) = forecast_rate(observed_rate, passive_rate);
        let projected_final_sr = project_final_sr(current_sr, forecast_rate, remaining_hours);

        entries.push(SeasonRaceEntry {
            guild_name,
            guild_prefix,
            current_sr,
            projected_final_sr,
            current_rank,
            projected_rank: current_rank,
            territory_count: current_territory_count,
            sample_count: u32::try_from(observations.len()).unwrap_or(u32::MAX),
            last_sampled_at: observed_at.to_rfc3339(),
            observed_rate_per_hour: observed_rate,
            passive_rate_per_hour: passive_rate,
            forecast_rate_per_hour: forecast_rate,
            forecast_source,
            series: downsample_series_hourly(observations),
        });
    }

    apply_projected_ranks(&mut entries);

    Ok(SeasonRaceResponse {
        season_id: active.season_id,
        label: active.label,
        start_at: active.start_at.to_rfc3339(),
        end_at: active.end_at.to_rfc3339(),
        generated_at: generated_at.to_rfc3339(),
        remaining_hours,
        entries,
        assumptions: SeasonRaceAssumptions {
            lookback_hours: active.lookback_hours,
            passive_scalar_weighted: passive_scalar,
            note: "Projection assumes the current pace continues from recent observed season rating snapshots."
                .to_string(),
        },
    })
}

async fn live_territory_counts(state: &AppState) -> HashMap<String, usize> {
    let snapshot = state.live_snapshot.read().await;
    let mut counts = HashMap::new();
    for territory in snapshot.territories.values() {
        *counts.entry(territory.guild.name.clone()).or_insert(0) += 1;
    }
    counts
}

async fn latest_scalar_sample_for_season(
    state: &AppState,
    season_id: i32,
) -> Option<SeasonScalarSample> {
    let latest = state.latest_scalar_sample.read().await;
    latest
        .as_ref()
        .map(|(sample, _)| sample.clone())
        .filter(|sample| sample.season_id == season_id)
}

async fn latest_scalar_weighted_for_season(state: &AppState, season_id: i32) -> Option<f64> {
    latest_scalar_sample_for_season(state, season_id)
        .await
        .map(|sample| sample.scalar_weighted)
}

fn observed_rate_per_hour(
    observations: &[SeriesObservation],
    season_start: DateTime<Utc>,
    generated_at: DateTime<Utc>,
    lookback_hours: i64,
) -> Option<f64> {
    if observations.len() < 2 {
        return None;
    }

    let lookback_start = season_start.max(generated_at - Duration::hours(lookback_hours.max(1)));
    let recent: Vec<&SeriesObservation> = observations
        .iter()
        .filter(|point| point.observed_at >= lookback_start)
        .collect();
    let window: Vec<&SeriesObservation> = if recent.len() >= 2 {
        recent
    } else {
        observations.iter().collect()
    };

    let first = *window.first()?;
    let last = *window.last()?;
    let elapsed_hours = (last.observed_at - first.observed_at).num_seconds() as f64 / 3600.0;
    if elapsed_hours <= 0.0 {
        return None;
    }

    Some((last.season_rating - first.season_rating) as f64 / elapsed_hours)
}

fn forecast_rate(observed_rate: Option<f64>, passive_rate: Option<f64>) -> (f64, ForecastSource) {
    if let Some(rate) = observed_rate {
        return (rate, ForecastSource::ObservedTrend);
    }
    if let Some(rate) = passive_rate {
        return (rate, ForecastSource::PassiveFallback);
    }
    (0.0, ForecastSource::FlatFallback)
}

fn project_final_sr(current_sr: i64, forecast_rate: f64, remaining_hours: f64) -> i64 {
    ((current_sr as f64) + forecast_rate * remaining_hours)
        .round()
        .max(0.0) as i64
}

fn downsample_series_hourly(observations: Vec<SeriesObservation>) -> Vec<SeasonRacePoint> {
    let mut downsampled: Vec<SeasonRacePoint> = Vec::new();
    let mut last_bucket: Option<i64> = None;
    let mut pending: Option<&SeriesObservation> = None;

    for observation in &observations {
        let bucket = observation.observed_at.timestamp() / 3600;
        match last_bucket {
            Some(current_bucket) if current_bucket != bucket => {
                if let Some(point) = pending.take() {
                    downsampled.push(SeasonRacePoint {
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
        downsampled.push(SeasonRacePoint {
            sampled_at: point.observed_at.to_rfc3339(),
            season_rating: point.season_rating,
        });
    }

    downsampled
}

fn apply_projected_ranks(entries: &mut [SeasonRaceEntry]) {
    let mut ranking: Vec<(usize, i64, usize, String)> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            (
                idx,
                entry.projected_final_sr,
                entry.territory_count,
                entry.guild_name.clone(),
            )
        })
        .collect();
    ranking.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
    });

    for (rank_idx, (entry_idx, _, _, _)) in ranking.into_iter().enumerate() {
        entries[entry_idx].projected_rank = u32::try_from(rank_idx + 1).unwrap_or(u32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ForecastSource, SeriesObservation, apply_projected_ranks, downsample_series_hourly,
        forecast_rate, observed_rate_per_hour, project_final_sr,
    };
    use chrono::{DateTime, Utc};

    fn ts(value: &str) -> DateTime<Utc> {
        value.parse::<DateTime<Utc>>().expect("parse timestamp")
    }

    #[test]
    fn observed_rate_uses_recent_window_when_available() {
        let points = vec![
            SeriesObservation {
                observed_at: ts("2026-03-01T00:00:00Z"),
                season_rating: 100,
            },
            SeriesObservation {
                observed_at: ts("2026-03-01T10:00:00Z"),
                season_rating: 220,
            },
            SeriesObservation {
                observed_at: ts("2026-03-01T12:00:00Z"),
                season_rating: 260,
            },
        ];

        let rate = observed_rate_per_hour(
            &points,
            ts("2026-03-01T00:00:00Z"),
            ts("2026-03-01T12:00:00Z"),
            6,
        )
        .expect("recent rate");

        assert!((rate - 20.0).abs() < 1e-9);
    }

    #[test]
    fn forecast_rate_prefers_observed_then_passive() {
        assert_eq!(
            forecast_rate(Some(12.0), Some(8.0)),
            (12.0, ForecastSource::ObservedTrend)
        );
        assert_eq!(
            forecast_rate(None, Some(8.0)),
            (8.0, ForecastSource::PassiveFallback)
        );
        assert_eq!(
            forecast_rate(None, None),
            (0.0, ForecastSource::FlatFallback)
        );
    }

    #[test]
    fn project_final_sr_clamps_to_zero() {
        assert_eq!(project_final_sr(100, -1000.0, 1.0), 0);
    }

    #[test]
    fn downsample_series_hourly_keeps_latest_point_per_hour() {
        let points = vec![
            SeriesObservation {
                observed_at: ts("2026-03-01T10:05:00Z"),
                season_rating: 100,
            },
            SeriesObservation {
                observed_at: ts("2026-03-01T10:45:00Z"),
                season_rating: 140,
            },
            SeriesObservation {
                observed_at: ts("2026-03-01T11:10:00Z"),
                season_rating: 150,
            },
        ];

        let downsampled = downsample_series_hourly(points);
        assert_eq!(downsampled.len(), 2);
        assert_eq!(downsampled[0].sampled_at, "2026-03-01T10:45:00+00:00");
        assert_eq!(downsampled[1].season_rating, 150);
    }

    #[test]
    fn apply_projected_ranks_orders_by_projection_then_territories() {
        let mut entries = vec![
            super::SeasonRaceEntry {
                guild_name: "Beta".to_string(),
                guild_prefix: "B".to_string(),
                current_sr: 0,
                projected_final_sr: 200,
                current_rank: 2,
                projected_rank: 0,
                territory_count: 10,
                sample_count: 0,
                last_sampled_at: "2026-03-01T00:00:00Z".to_string(),
                observed_rate_per_hour: None,
                passive_rate_per_hour: None,
                forecast_rate_per_hour: 0.0,
                forecast_source: ForecastSource::FlatFallback,
                series: Vec::new(),
            },
            super::SeasonRaceEntry {
                guild_name: "Alpha".to_string(),
                guild_prefix: "A".to_string(),
                current_sr: 0,
                projected_final_sr: 200,
                current_rank: 1,
                projected_rank: 0,
                territory_count: 12,
                sample_count: 0,
                last_sampled_at: "2026-03-01T00:00:00Z".to_string(),
                observed_rate_per_hour: None,
                passive_rate_per_hour: None,
                forecast_rate_per_hour: 0.0,
                forecast_source: ForecastSource::FlatFallback,
                series: Vec::new(),
            },
        ];

        apply_projected_ranks(&mut entries);
        assert_eq!(entries[0].projected_rank, 2);
        assert_eq!(entries[1].projected_rank, 1);
    }
}
