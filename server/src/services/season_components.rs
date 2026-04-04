use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sequoia_shared::passive_sr_per_hour;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config;
use crate::services::season_data::ResolvedSeasonWindow;
use crate::state::AppState;

type ObservationRow = (String, DateTime<Utc>, i32, i16);
type ScalarRow = (DateTime<Utc>, f64);

#[derive(Debug, Clone, Serialize)]
pub struct SeasonComponentPoint {
    pub sampled_at: String,
    pub value: i64,
}

#[derive(Debug, Clone)]
pub struct GuildSeasonComponents {
    pub daily_raid_count_series: Vec<SeasonComponentPoint>,
    pub daily_raid_sr_series: Vec<SeasonComponentPoint>,
    pub cumulative_raid_sr_series: Vec<SeasonComponentPoint>,
    pub cumulative_passive_hold_sr_series: Vec<SeasonComponentPoint>,
    pub cumulative_conquest_sr_series: Vec<SeasonComponentPoint>,
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectedSeasonComponents {
    pub projected_raid_sr: i64,
    pub projected_passive_hold_sr: i64,
    pub projected_conquest_sr: i64,
}

#[derive(Debug, Clone)]
struct GuildObservation {
    observed_at: DateTime<Utc>,
    season_rating: i64,
    territory_count: usize,
}

#[derive(Debug, Clone, Default)]
struct RaidActivityDay {
    total_count: i64,
    estimated_sr_gain: i64,
}

#[derive(Debug, Clone, Default)]
struct RaidActivityEntry {
    by_day: BTreeMap<NaiveDate, RaidActivityDay>,
}

#[derive(Debug, Deserialize)]
struct SeasonRaidActivityResponse {
    entries: Vec<SeasonRaidActivityEntryResponse>,
}

#[derive(Debug, Deserialize)]
struct SeasonRaidActivityEntryResponse {
    guild_name: String,
    days: Vec<SeasonRaidActivityDayResponse>,
}

#[derive(Debug, Deserialize)]
struct SeasonRaidActivityDayResponse {
    day: String,
    raid_counts: Vec<RaidCountResponse>,
}

#[derive(Debug, Deserialize)]
struct RaidCountResponse {
    count: i64,
}

pub async fn build_components(
    state: &AppState,
    window: &ResolvedSeasonWindow,
    guild_names: &[String],
    range_end: DateTime<Utc>,
) -> Result<HashMap<String, GuildSeasonComponents>, String> {
    let Some(pool) = state.db.as_ref() else {
        return Err("database unavailable".to_string());
    };
    if guild_names.is_empty() {
        return Ok(HashMap::new());
    }

    let normalized_names = guild_names
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();

    let observation_rows: Vec<ObservationRow> = sqlx::query_as(
        "SELECT guild_name, observed_at, season_rating, territory_count \
         FROM season_guild_observations \
         WHERE season_id = $1 \
           AND LOWER(guild_name) = ANY($2) \
           AND observed_at >= $3 \
           AND observed_at <= $4 \
         ORDER BY LOWER(guild_name) ASC, observed_at ASC",
    )
    .bind(window.season_id)
    .bind(&normalized_names)
    .bind(window.start_at)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load season observations: {e}"))?;

    let scalar_rows: Vec<ScalarRow> = sqlx::query_as(
        "SELECT sampled_at, scalar_weighted \
         FROM season_scalar_samples \
         WHERE season_id = $1 \
           AND sampled_at <= $2 \
         ORDER BY sampled_at ASC",
    )
    .bind(window.season_id)
    .bind(range_end)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load scalar samples: {e}"))?;

    let raid_activity = fetch_raid_activity(state, window, guild_names, range_end).await;

    let mut observations_by_name: HashMap<String, Vec<GuildObservation>> = HashMap::new();
    for (guild_name, observed_at, season_rating, territory_count) in observation_rows {
        observations_by_name
            .entry(guild_name.to_ascii_lowercase())
            .or_default()
            .push(GuildObservation {
                observed_at,
                season_rating: i64::from(season_rating),
                territory_count: usize::try_from(territory_count.max(0)).unwrap_or(0),
            });
    }

    let start_day = window.start_at.date_naive();
    let observed_day = range_end.date_naive();
    let mut components_by_name = HashMap::new();
    for guild_name in guild_names {
        let key = guild_name.trim().to_ascii_lowercase();
        let observations = observations_by_name.remove(&key).unwrap_or_default();
        let total_by_day = fill_daily_totals(&observations, start_day, observed_day);
        let passive_by_day = integrate_passive_daily(
            &observations,
            &scalar_rows,
            window.start_at,
            range_end,
            start_day,
            observed_day,
        );
        let raid_entry = raid_activity.get(&key).cloned().unwrap_or_default();
        components_by_name.insert(
            key,
            build_component_series(
                total_by_day,
                passive_by_day,
                raid_entry,
                start_day,
                observed_day,
            ),
        );
    }

    Ok(components_by_name)
}

pub fn project_components(
    components: &GuildSeasonComponents,
    projected_passive_hold_gain: Option<i64>,
    remaining_hours: f64,
    conquest_half_life_hours: f64,
) -> ProjectedSeasonComponents {
    let current_raid_sr = current_raid_sr(components);
    let current_passive_hold_sr = current_passive_hold_sr(components);
    let current_conquest_sr = current_conquest_sr(components);
    let projected_passive_hold_gain = projected_passive_hold_gain.unwrap_or(0).max(0);
    let projected_raid_gain = projected_raid_gain(components, remaining_hours);
    let projected_conquest_gain =
        projected_conquest_gain(components, remaining_hours, conquest_half_life_hours);

    ProjectedSeasonComponents {
        projected_raid_sr: current_raid_sr.saturating_add(projected_raid_gain),
        projected_passive_hold_sr: current_passive_hold_sr
            .saturating_add(projected_passive_hold_gain),
        projected_conquest_sr: current_conquest_sr.saturating_add(projected_conquest_gain),
    }
}

pub fn current_raid_sr(components: &GuildSeasonComponents) -> i64 {
    components
        .cumulative_raid_sr_series
        .last()
        .map(|point| point.value)
        .unwrap_or(0)
}

pub fn current_passive_hold_sr(components: &GuildSeasonComponents) -> i64 {
    components
        .cumulative_passive_hold_sr_series
        .last()
        .map(|point| point.value)
        .unwrap_or(0)
}

pub fn current_conquest_sr(components: &GuildSeasonComponents) -> i64 {
    components
        .cumulative_conquest_sr_series
        .last()
        .map(|point| point.value)
        .unwrap_or(0)
}

async fn fetch_raid_activity(
    state: &AppState,
    window: &ResolvedSeasonWindow,
    guild_names: &[String],
    range_end: DateTime<Utc>,
) -> HashMap<String, RaidActivityEntry> {
    let Some(base_url) = config::sequoia_backend_base_url() else {
        return HashMap::new();
    };
    let url = format!("{base_url}/api/season/raid-activity");
    let response = match state
        .http_client
        .get(url)
        .query(&[
            ("from", window.start_at.to_rfc3339()),
            ("to", range_end.to_rfc3339()),
            ("guild_names", guild_names.join(",")),
        ])
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            warn!("failed to load internal raid activity: {error}");
            return HashMap::new();
        }
    };

    let payload = match response.error_for_status() {
        Ok(response) => match response.json::<SeasonRaidActivityResponse>().await {
            Ok(payload) => payload,
            Err(error) => {
                warn!("failed to decode internal raid activity: {error}");
                return HashMap::new();
            }
        },
        Err(error) => {
            warn!("internal raid activity request failed: {error}");
            return HashMap::new();
        }
    };

    let players_per_completion = config::season_raid_players_per_completion();
    let sr_per_completion = config::season_raid_sr_per_completion();

    let mut by_name = HashMap::new();
    for entry in payload.entries {
        let mut by_day = BTreeMap::new();
        for day in entry.days {
            let Ok(day_date) = NaiveDate::parse_from_str(&day.day, "%Y-%m-%d") else {
                continue;
            };
            let total_count = day
                .raid_counts
                .iter()
                .map(|raid_count| raid_count.count.max(0))
                .sum::<i64>();
            let normalized_count = normalize_raid_count(total_count, players_per_completion);
            let estimated_sr_gain =
                estimate_raid_sr_gain(total_count, players_per_completion, sr_per_completion);
            by_day.insert(
                day_date,
                RaidActivityDay {
                    total_count: normalized_count,
                    estimated_sr_gain,
                },
            );
        }
        by_name.insert(
            entry.guild_name.to_ascii_lowercase(),
            RaidActivityEntry { by_day },
        );
    }
    by_name
}

fn normalize_raid_count(total_player_completions: i64, players_per_completion: f64) -> i64 {
    if total_player_completions <= 0 {
        return 0;
    }
    ((total_player_completions as f64) / players_per_completion)
        .round()
        .max(0.0) as i64
}

fn estimate_raid_sr_gain(
    total_player_completions: i64,
    players_per_completion: f64,
    sr_per_completion: f64,
) -> i64 {
    if total_player_completions <= 0 {
        return 0;
    }
    (((total_player_completions as f64) / players_per_completion) * sr_per_completion)
        .round()
        .max(0.0) as i64
}

fn fill_daily_totals(
    observations: &[GuildObservation],
    start_day: NaiveDate,
    observed_day: NaiveDate,
) -> Vec<(NaiveDate, i64)> {
    let mut max_by_day = BTreeMap::new();
    for observation in observations {
        max_by_day
            .entry(observation.observed_at.date_naive())
            .and_modify(|current: &mut i64| *current = (*current).max(observation.season_rating))
            .or_insert(observation.season_rating);
    }

    let mut carried_total = 0i64;
    let mut results = Vec::new();
    let mut day = start_day;
    while day <= observed_day {
        carried_total = carried_total.max(*max_by_day.get(&day).unwrap_or(&carried_total));
        results.push((day, carried_total));
        day = day.succ_opt().expect("advance date");
    }
    results
}

fn integrate_passive_daily(
    observations: &[GuildObservation],
    scalar_rows: &[ScalarRow],
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    start_day: NaiveDate,
    observed_day: NaiveDate,
) -> BTreeMap<NaiveDate, i64> {
    let mut passive_by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    if start_at >= end_at {
        return zero_days(start_day, observed_day);
    }

    let mut current_territory_count = observations
        .first()
        .map(|observation| observation.territory_count)
        .unwrap_or(0);
    let mut current_at = start_at;

    for observation in observations {
        if observation.observed_at <= start_at {
            current_territory_count = observation.territory_count;
            continue;
        }
        let segment_end = observation.observed_at.min(end_at);
        if segment_end > current_at {
            add_passive_interval(
                &mut passive_by_day,
                current_at,
                segment_end,
                current_territory_count,
                scalar_rows,
            );
            current_at = segment_end;
        }
        current_territory_count = observation.territory_count;
        if current_at >= end_at {
            break;
        }
    }

    if current_at < end_at {
        add_passive_interval(
            &mut passive_by_day,
            current_at,
            end_at,
            current_territory_count,
            scalar_rows,
        );
    }

    let mut results = zero_days(start_day, observed_day);
    for (day, value) in passive_by_day {
        results.insert(day, value.round().max(0.0) as i64);
    }
    results
}

fn add_passive_interval(
    passive_by_day: &mut BTreeMap<NaiveDate, f64>,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    territory_count: usize,
    scalar_rows: &[ScalarRow],
) {
    if territory_count == 0 || start_at >= end_at {
        return;
    }

    let mut cursor = start_at;
    while cursor < end_at {
        let next_day = cursor
            .date_naive()
            .succ_opt()
            .and_then(|day| day.and_hms_opt(0, 0, 0))
            .map(|naive| Utc.from_utc_datetime(&naive))
            .unwrap_or(end_at);
        let segment_end = end_at.min(next_day);
        let scalar = scalar_at(scalar_rows, cursor);
        let hours = (segment_end - cursor).num_seconds() as f64 / 3600.0;
        *passive_by_day.entry(cursor.date_naive()).or_insert(0.0) +=
            passive_sr_per_hour(territory_count, scalar) * hours;
        cursor = segment_end;
    }
}

fn build_component_series(
    total_by_day: Vec<(NaiveDate, i64)>,
    passive_by_day: BTreeMap<NaiveDate, i64>,
    raid_entry: RaidActivityEntry,
    start_day: NaiveDate,
    observed_day: NaiveDate,
) -> GuildSeasonComponents {
    let mut daily_raid_count_series = Vec::new();
    let mut daily_raid_sr_series = Vec::new();
    let mut cumulative_raid_sr_series = Vec::new();
    let mut cumulative_passive_hold_sr_series = Vec::new();
    let mut cumulative_conquest_sr_series = Vec::new();

    let mut previous_total = 0i64;
    let mut cumulative_raid = 0i64;
    let mut cumulative_passive = 0i64;
    let mut cumulative_conquest = 0i64;

    for (day, total_value) in total_by_day {
        let raid_day = raid_entry.by_day.get(&day).cloned().unwrap_or_default();
        let total_gain = (total_value - previous_total).max(0);
        let passive_gain_raw = passive_by_day.get(&day).copied().unwrap_or(0).max(0);
        let passive_gain = passive_gain_raw.min(total_gain);
        let remaining_after_passive = total_gain.saturating_sub(passive_gain);
        let raid_sr_gain = raid_day
            .estimated_sr_gain
            .min(remaining_after_passive)
            .max(0);
        let conquest_gain = remaining_after_passive.saturating_sub(raid_sr_gain);

        cumulative_raid += raid_sr_gain;
        cumulative_passive += passive_gain;
        cumulative_conquest += conquest_gain;

        let sampled_at = day
            .and_hms_opt(0, 0, 0)
            .map(|naive| Utc.from_utc_datetime(&naive).to_rfc3339())
            .expect("daily timestamp");

        daily_raid_count_series.push(SeasonComponentPoint {
            sampled_at: sampled_at.clone(),
            value: raid_day.total_count.max(0),
        });
        daily_raid_sr_series.push(SeasonComponentPoint {
            sampled_at: sampled_at.clone(),
            value: raid_sr_gain,
        });
        cumulative_raid_sr_series.push(SeasonComponentPoint {
            sampled_at: sampled_at.clone(),
            value: cumulative_raid,
        });
        cumulative_passive_hold_sr_series.push(SeasonComponentPoint {
            sampled_at: sampled_at.clone(),
            value: cumulative_passive,
        });
        cumulative_conquest_sr_series.push(SeasonComponentPoint {
            sampled_at,
            value: cumulative_conquest,
        });

        previous_total = total_value;
    }

    if daily_raid_count_series.is_empty() {
        let mut day = start_day;
        while day <= observed_day {
            let sampled_at = day
                .and_hms_opt(0, 0, 0)
                .map(|naive| Utc.from_utc_datetime(&naive).to_rfc3339())
                .expect("daily timestamp");
            daily_raid_count_series.push(SeasonComponentPoint {
                sampled_at: sampled_at.clone(),
                value: 0,
            });
            daily_raid_sr_series.push(SeasonComponentPoint {
                sampled_at: sampled_at.clone(),
                value: 0,
            });
            cumulative_raid_sr_series.push(SeasonComponentPoint {
                sampled_at: sampled_at.clone(),
                value: 0,
            });
            cumulative_passive_hold_sr_series.push(SeasonComponentPoint {
                sampled_at: sampled_at.clone(),
                value: 0,
            });
            cumulative_conquest_sr_series.push(SeasonComponentPoint {
                sampled_at,
                value: 0,
            });
            day = day.succ_opt().expect("advance date");
        }
    }

    GuildSeasonComponents {
        daily_raid_count_series,
        daily_raid_sr_series,
        cumulative_raid_sr_series,
        cumulative_passive_hold_sr_series,
        cumulative_conquest_sr_series,
    }
}

fn scalar_at(samples: &[ScalarRow], timestamp: DateTime<Utc>) -> f64 {
    let mut latest = None;
    for (sampled_at, scalar_weighted) in samples {
        if *sampled_at <= timestamp {
            latest = Some(*scalar_weighted);
        } else {
            break;
        }
    }
    latest
        .or_else(|| samples.first().map(|(_, scalar_weighted)| *scalar_weighted))
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0)
}

fn zero_days(start_day: NaiveDate, observed_day: NaiveDate) -> BTreeMap<NaiveDate, i64> {
    let mut zeros = BTreeMap::new();
    let mut day = start_day;
    while day <= observed_day {
        zeros.insert(day, 0);
        day = day.succ_opt().expect("advance date");
    }
    zeros
}

fn ewma_last_n(series: &[SeasonComponentPoint], count: usize) -> f64 {
    if series.is_empty() {
        return 0.0;
    }
    let start_idx = series.len().saturating_sub(count);
    let values = series[start_idx..]
        .iter()
        .map(|point| point.value as f64)
        .collect::<Vec<_>>();
    let alpha = 2.0 / (values.len() as f64 + 1.0);
    let mut current = values[0];
    for value in values.into_iter().skip(1) {
        current = alpha * value + (1.0 - alpha) * current;
    }
    current
}

pub fn projected_raid_gain(components: &GuildSeasonComponents, remaining_hours: f64) -> i64 {
    if remaining_hours <= 0.0 {
        return 0;
    }
    let remaining_days = remaining_hours / 24.0;
    let ewma_daily_raid_sr = ewma_last_n(&components.daily_raid_sr_series, 7);
    (ewma_daily_raid_sr * remaining_days).round().max(0.0) as i64
}

pub fn projected_conquest_gain(
    components: &GuildSeasonComponents,
    remaining_hours: f64,
    conquest_half_life_hours: f64,
) -> i64 {
    if remaining_hours <= 0.0 || conquest_half_life_hours <= 0.0 {
        return 0;
    }

    let daily_conquest_sr_series = daily_delta_series(&components.cumulative_conquest_sr_series);
    let conquest_rate_per_hour = ewma_last_n(&daily_conquest_sr_series, 7) / 24.0;
    if !conquest_rate_per_hour.is_finite() || conquest_rate_per_hour <= 0.0 {
        return 0;
    }

    let lambda = std::f64::consts::LN_2 / conquest_half_life_hours;
    let gain = conquest_rate_per_hour * ((1.0 - (-lambda * remaining_hours).exp()) / lambda);
    gain.round().max(0.0) as i64
}

fn daily_delta_series(series: &[SeasonComponentPoint]) -> Vec<SeasonComponentPoint> {
    let mut previous = 0i64;
    let mut deltas = Vec::with_capacity(series.len());
    for point in series {
        let value = point.value.saturating_sub(previous).max(0);
        deltas.push(SeasonComponentPoint {
            sampled_at: point.sampled_at.clone(),
            value,
        });
        previous = point.value;
    }
    deltas
}

#[cfg(test)]
mod tests {
    use super::{
        GuildSeasonComponents, SeasonComponentPoint, build_component_series, estimate_raid_sr_gain,
        normalize_raid_count, project_components, projected_conquest_gain, projected_raid_gain,
    };
    use chrono::NaiveDate;
    use std::collections::BTreeMap;

    fn point(day: &str, value: i64) -> SeasonComponentPoint {
        SeasonComponentPoint {
            sampled_at: format!("{day}T00:00:00+00:00"),
            value,
        }
    }

    #[test]
    fn build_component_series_bounds_raid_and_passive_to_total_gain() {
        let total_by_day = vec![
            (NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"), 100),
            (NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"), 220),
        ];
        let passive_by_day = BTreeMap::from([
            (NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"), 40),
            (NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"), 90),
        ]);
        let mut raid_entry = super::RaidActivityEntry::default();
        raid_entry.by_day.insert(
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"),
            super::RaidActivityDay {
                total_count: 4,
                estimated_sr_gain: 80,
            },
        );
        raid_entry.by_day.insert(
            NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"),
            super::RaidActivityDay {
                total_count: 6,
                estimated_sr_gain: 200,
            },
        );

        let components = build_component_series(
            total_by_day,
            passive_by_day,
            raid_entry,
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"),
            NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"),
        );

        assert_eq!(
            components
                .cumulative_passive_hold_sr_series
                .last()
                .expect("passive series")
                .value,
            130
        );
        assert_eq!(
            components
                .cumulative_raid_sr_series
                .last()
                .expect("raid series")
                .value,
            90
        );
        assert_eq!(
            components
                .cumulative_conquest_sr_series
                .last()
                .expect("conquest series")
                .value,
            0
        );
    }

    #[test]
    fn project_components_projects_passive_raid_and_conquest_independently() {
        let components = GuildSeasonComponents {
            daily_raid_count_series: vec![point("2026-03-01", 2), point("2026-03-02", 3)],
            daily_raid_sr_series: vec![point("2026-03-01", 50), point("2026-03-02", 60)],
            cumulative_raid_sr_series: vec![point("2026-03-01", 50), point("2026-03-02", 110)],
            cumulative_passive_hold_sr_series: vec![
                point("2026-03-01", 30),
                point("2026-03-02", 80),
            ],
            cumulative_conquest_sr_series: vec![point("2026-03-01", 20), point("2026-03-02", 60)],
        };

        let projected = project_components(&components, Some(70), 48.0, 72.0);

        assert_eq!(projected.projected_passive_hold_sr, 150);
        assert_eq!(projected.projected_raid_sr, 223);
        assert_eq!(projected.projected_conquest_sr, 113);
    }

    #[test]
    fn projected_raid_gain_uses_recent_daily_raid_sr() {
        let components = GuildSeasonComponents {
            daily_raid_count_series: vec![point("2026-03-01", 0), point("2026-03-02", 0)],
            daily_raid_sr_series: vec![point("2026-03-01", 50), point("2026-03-02", 70)],
            cumulative_raid_sr_series: vec![point("2026-03-01", 50), point("2026-03-02", 120)],
            cumulative_passive_hold_sr_series: vec![
                point("2026-03-01", 30),
                point("2026-03-02", 80),
            ],
            cumulative_conquest_sr_series: vec![point("2026-03-01", 20), point("2026-03-02", 60)],
        };

        assert_eq!(projected_raid_gain(&components, 48.0), 127);
    }

    #[test]
    fn projected_conquest_gain_decays_recent_conquest_sr() {
        let components = GuildSeasonComponents {
            daily_raid_count_series: vec![point("2026-03-01", 0), point("2026-03-02", 0)],
            daily_raid_sr_series: vec![point("2026-03-01", 0), point("2026-03-02", 0)],
            cumulative_raid_sr_series: vec![point("2026-03-01", 0), point("2026-03-02", 0)],
            cumulative_passive_hold_sr_series: vec![
                point("2026-03-01", 30),
                point("2026-03-02", 80),
            ],
            cumulative_conquest_sr_series: vec![point("2026-03-01", 20), point("2026-03-02", 80)],
        };

        assert!(projected_conquest_gain(&components, 72.0, 72.0) > 0);
    }

    #[test]
    fn normalized_raid_metrics_convert_player_completions_to_guild_completions() {
        assert_eq!(normalize_raid_count(0, 4.0), 0);
        assert_eq!(normalize_raid_count(10, 4.0), 3);
        assert_eq!(estimate_raid_sr_gain(10, 4.0, 380.0), 950);
    }

    #[test]
    fn build_component_series_leaves_conquest_when_raid_estimate_is_reasonable() {
        let total_by_day = vec![
            (NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"), 200),
            (NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"), 400),
        ];
        let passive_by_day = BTreeMap::from([
            (NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"), 50),
            (NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"), 75),
        ]);
        let mut raid_entry = super::RaidActivityEntry::default();
        raid_entry.by_day.insert(
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"),
            super::RaidActivityDay {
                total_count: 2,
                estimated_sr_gain: 80,
            },
        );
        raid_entry.by_day.insert(
            NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"),
            super::RaidActivityDay {
                total_count: 3,
                estimated_sr_gain: 90,
            },
        );

        let components = build_component_series(
            total_by_day,
            passive_by_day,
            raid_entry,
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("day"),
            NaiveDate::from_ymd_opt(2026, 3, 2).expect("day"),
        );

        assert!(
            components
                .cumulative_conquest_sr_series
                .last()
                .expect("conquest series")
                .value
                > 0
        );
    }
}
