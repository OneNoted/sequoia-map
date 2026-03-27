use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use sequoia_shared::{SeasonScalarSample, passive_sr_per_hour};

use crate::config;
use crate::services::season_data::{self, SeasonDataError};
use crate::services::season_scalar_forecast::{self, ScalarProjection};
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
    ScalarProjection,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_passive_sr_gain: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_excess_sr_gain: Option<i64>,
    pub forecast_rate_per_hour: f64,
    pub forecast_source: ForecastSource,
    pub series: Vec<SeasonRacePoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRaceAssumptions {
    pub lookback_hours: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_scalar_weighted: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_scalar_weighted: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum_half_life_hours: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonRaceScalarPoint {
    pub sampled_at: String,
    pub scalar_weighted: f64,
    pub source: season_scalar_forecast::ScalarPointSource,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scalar_points: Vec<SeasonRaceScalarPoint>,
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
    let season_windows = season_data::list_resolved_windows(state)
        .await
        .map_err(map_season_data_error)?;
    let window = season_data::resolve_requested_window(state, requested_season_id)
        .await
        .map_err(map_season_data_error)?
        .ok_or(SeasonRaceError::Unavailable)?;
    let scalar_override_points =
        config::season_scalar_override_points().map_err(|_| SeasonRaceError::Internal)?;
    let lookback_hours = config::season_race_lookback_hours();
    let top_guilds = config::season_race_top_guilds();

    let generated_at = Utc::now();
    let range_end = generated_at.min(window.end_at);
    let remaining_hours = ((window.end_at - generated_at).num_seconds().max(0) as f64) / 3600.0;
    let recent_query_start =
        recent_observation_query_start(window.start_at, range_end, lookback_hours);
    let season_complete = generated_at >= window.end_at;
    let scalar_projection = season_scalar_forecast::build_scalar_projection(
        pool,
        &season_windows,
        &window,
        generated_at,
        &scalar_override_points,
    )
    .await
    .map_err(|_| SeasonRaceError::Internal)?;
    let fallback_scalar = latest_scalar_weighted_for_season(state, window.season_id).await;
    let current_scalar_assumption = scalar_projection
        .as_ref()
        .map(ScalarProjection::current_scalar_weighted)
        .or(fallback_scalar);

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
    .bind(window.season_id)
    .bind(window.start_at)
    .bind(range_end)
    .bind(i64::try_from(top_guilds).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonRaceError::Internal)?;

    if latest_rows.is_empty() {
        return Ok(SeasonRaceResponse {
            season_id: window.season_id,
            label: window.label.clone(),
            start_at: window.start_at.to_rfc3339(),
            end_at: window.end_at.to_rfc3339(),
            generated_at: generated_at.to_rfc3339(),
            remaining_hours,
            scalar_points: scalar_points(&scalar_projection),
            entries: Vec::new(),
            assumptions: SeasonRaceAssumptions {
                lookback_hours,
                passive_scalar_weighted: current_scalar_assumption,
                current_scalar_weighted: scalar_projection
                    .as_ref()
                    .map(ScalarProjection::current_scalar_weighted),
                momentum_half_life_hours: scalar_projection
                    .as_ref()
                    .map(|_| season_scalar_forecast::momentum_half_life_hours()),
                note: assumptions_note(season_complete, scalar_projection.as_ref()).to_string(),
            },
        });
    }

    let guild_names: Vec<String> = latest_rows.iter().map(|row| row.0.clone()).collect();
    let recent_rows: Vec<SeriesRow> = sqlx::query_as(
        "SELECT guild_name, observed_at, season_rating, territory_count \
         FROM season_guild_observations \
         WHERE season_id = $1 \
           AND guild_name = ANY($2) \
           AND observed_at >= $3 \
           AND observed_at <= $4 \
         ORDER BY guild_name ASC, observed_at ASC",
    )
    .bind(window.season_id)
    .bind(&guild_names)
    .bind(recent_query_start)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonRaceError::Internal)?;

    let chart_rows: Vec<SeriesRow> = sqlx::query_as(
        "SELECT guild_name, observed_at, season_rating, territory_count \
         FROM ( \
             SELECT DISTINCT ON (guild_name, date_trunc('hour', observed_at)) \
                 guild_name, observed_at, season_rating, territory_count \
             FROM season_guild_observations \
             WHERE season_id = $1 \
               AND guild_name = ANY($2) \
               AND observed_at >= $3 \
               AND observed_at <= $4 \
             ORDER BY guild_name, date_trunc('hour', observed_at), observed_at DESC \
         ) hourly \
         ORDER BY guild_name ASC, observed_at ASC",
    )
    .bind(window.season_id)
    .bind(&guild_names)
    .bind(window.start_at)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|_| SeasonRaceError::Internal)?;

    let mut recent_by_guild: HashMap<String, Vec<SeriesObservation>> = HashMap::new();
    let mut latest_observed_territory_count: HashMap<String, usize> = HashMap::new();
    for (guild_name, observed_at, season_rating, territory_count) in recent_rows {
        recent_by_guild
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

    let mut series_by_guild: HashMap<String, Vec<SeriesObservation>> = HashMap::new();
    for (guild_name, observed_at, season_rating, _territory_count) in chart_rows {
        series_by_guild
            .entry(guild_name)
            .or_default()
            .push(SeriesObservation {
                observed_at,
                season_rating: i64::from(season_rating),
            });
    }

    let live_territory_counts = live_territory_counts(state).await;
    let passive_scalar = latest_scalar_sample_for_season(state, window.season_id)
        .await
        .map(|sample| sample.scalar_weighted);
    let current_scalar = scalar_projection
        .as_ref()
        .map(ScalarProjection::current_scalar_weighted)
        .or(passive_scalar);

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
        let recent_observations = recent_by_guild.remove(&guild_name).unwrap_or_default();
        let chart_observations = series_by_guild.remove(&guild_name).unwrap_or_default();
        let observed_rate = observed_rate_per_hour(
            &recent_observations,
            window.start_at,
            range_end,
            lookback_hours,
        );
        let passive_rate =
            current_scalar.map(|scalar| passive_sr_per_hour(current_territory_count, scalar));
        let (
            projected_final_sr,
            projected_passive_sr_gain,
            projected_excess_sr_gain,
            forecast_rate,
            forecast_source,
        ) = if let Some(projection) = scalar_projection.as_ref() {
            let passive_gain = projection.projected_passive_gain(
                current_territory_count,
                generated_at,
                window.end_at,
            );
            let excess_rate = observed_rate
                .zip(passive_rate)
                .map(|(observed, passive)| (observed - passive).max(0.0))
                .unwrap_or(0.0);
            let excess_gain =
                season_scalar_forecast::project_momentum_gain(excess_rate, remaining_hours);
            let projected_final_sr = current_sr
                .saturating_add(passive_gain)
                .saturating_add(excess_gain);
            let forecast_rate = if remaining_hours > 0.0 {
                (passive_gain + excess_gain) as f64 / remaining_hours
            } else {
                0.0
            };
            (
                projected_final_sr,
                Some(passive_gain),
                Some(excess_gain),
                forecast_rate,
                ForecastSource::ScalarProjection,
            )
        } else {
            let (forecast_rate, forecast_source) = forecast_rate(observed_rate, passive_rate);
            (
                project_final_sr(current_sr, forecast_rate, remaining_hours),
                None,
                None,
                forecast_rate,
                forecast_source,
            )
        };

        entries.push(SeasonRaceEntry {
            guild_name,
            guild_prefix,
            current_sr,
            projected_final_sr,
            current_rank,
            projected_rank: current_rank,
            territory_count: current_territory_count,
            sample_count: u32::try_from(chart_observations.len()).unwrap_or(u32::MAX),
            last_sampled_at: observed_at.to_rfc3339(),
            observed_rate_per_hour: observed_rate,
            passive_rate_per_hour: passive_rate,
            projected_passive_sr_gain,
            projected_excess_sr_gain,
            forecast_rate_per_hour: forecast_rate,
            forecast_source,
            series: downsample_series_hourly(chart_observations),
        });
    }

    apply_projected_ranks(&mut entries);

    Ok(SeasonRaceResponse {
        season_id: window.season_id,
        label: window.label,
        start_at: window.start_at.to_rfc3339(),
        end_at: window.end_at.to_rfc3339(),
        generated_at: generated_at.to_rfc3339(),
        remaining_hours,
        scalar_points: scalar_points(&scalar_projection),
        entries,
        assumptions: SeasonRaceAssumptions {
            lookback_hours,
            passive_scalar_weighted: current_scalar,
            current_scalar_weighted: scalar_projection
                .as_ref()
                .map(ScalarProjection::current_scalar_weighted),
            momentum_half_life_hours: scalar_projection
                .as_ref()
                .map(|_| season_scalar_forecast::momentum_half_life_hours()),
            note: assumptions_note(season_complete, scalar_projection.as_ref()).to_string(),
        },
    })
}

fn map_season_data_error(error: SeasonDataError) -> SeasonRaceError {
    match error {
        SeasonDataError::Unavailable => SeasonRaceError::Unavailable,
        SeasonDataError::BadRequest => SeasonRaceError::BadRequest,
        SeasonDataError::Internal => SeasonRaceError::Internal,
    }
}

fn assumptions_note(
    season_complete: bool,
    scalar_projection: Option<&ScalarProjection>,
) -> &'static str {
    if season_complete {
        "Season is complete; projected values equal the latest observed final season rating."
    } else if let Some(projection) = scalar_projection {
        if projection.uses_manual_override_points() {
            "Projection uses manual scalar overrides when provided, keeps territory count flat, and damps above-passive pace over 72 hours."
        } else if projection.uses_estimated_points() {
            "Projection estimates future scalar growth from observed season samples, keeps territory count flat, and damps above-passive pace over 72 hours."
        } else {
            "Projection uses the current observed scalar and flat territory count because future scalar transitions are not observed yet."
        }
    } else {
        "Projection assumes the current pace continues from recent observed season rating snapshots."
    }
}

fn scalar_points(projection: &Option<ScalarProjection>) -> Vec<SeasonRaceScalarPoint> {
    projection
        .as_ref()
        .map(|projection| {
            projection
                .api_points()
                .into_iter()
                .map(|point| SeasonRaceScalarPoint {
                    sampled_at: point.sampled_at,
                    scalar_weighted: point.scalar_weighted,
                    source: point.source,
                })
                .collect()
        })
        .unwrap_or_default()
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

fn recent_observation_query_start(
    season_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
    lookback_hours: i64,
) -> DateTime<Utc> {
    let buffered_hours = lookback_hours.max(1).saturating_mul(2);
    season_start.max(range_end - Duration::hours(buffered_hours))
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
        forecast_rate, observed_rate_per_hour, project_final_sr, recent_observation_query_start,
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
    fn recent_observation_query_start_uses_buffered_lookback_window() {
        assert_eq!(
            recent_observation_query_start(
                ts("2026-03-01T00:00:00Z"),
                ts("2026-03-02T12:00:00Z"),
                6,
            ),
            ts("2026-03-02T00:00:00Z")
        );
    }

    #[test]
    fn recent_observation_query_start_clamps_to_season_start() {
        assert_eq!(
            recent_observation_query_start(
                ts("2026-03-01T10:00:00Z"),
                ts("2026-03-01T12:00:00Z"),
                6,
            ),
            ts("2026-03-01T10:00:00Z")
        );
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
                projected_passive_sr_gain: None,
                projected_excess_sr_gain: None,
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
                projected_passive_sr_gain: None,
                projected_excess_sr_gain: None,
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
