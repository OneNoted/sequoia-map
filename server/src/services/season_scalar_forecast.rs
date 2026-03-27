use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use sequoia_shared::passive_sr_per_hour;

use crate::config::SeasonScalarOverridePoint;
use crate::services::season_data::ResolvedSeasonWindow;

const PROGRESS_BUCKETS: usize = 96;
const MIN_SAMPLE_WEIGHT: f64 = 0.05;
const LEVEL_EPSILON: f64 = 0.05;
const MOMENTUM_HALF_LIFE_HOURS: f64 = 72.0;
const SCALAR_LEVELS: [f64; 6] = [1.0, 1.5, 2.0, 3.0, 5.0, 10.0];

type ScalarSampleRow = (i32, DateTime<Utc>, f64, f64, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarPointSource {
    Observed,
    Estimated,
    ManualOverride,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeasonScalarPoint {
    pub sampled_at: String,
    pub scalar_weighted: f64,
    pub source: ScalarPointSource,
}

#[derive(Debug, Clone)]
pub struct ScalarProjection {
    current_scalar_weighted: f64,
    points: Vec<ScalarPointInternal>,
}

#[derive(Debug, Clone)]
struct ScalarPointInternal {
    sampled_at: DateTime<Utc>,
    scalar_weighted: f64,
    source: ScalarPointSource,
}

#[derive(Debug, Clone, Default)]
struct SeasonScalarCurve {
    latest: Option<ScalarPointInternal>,
    bucket_values: Vec<Option<f64>>,
}

pub async fn build_scalar_projection(
    pool: &sqlx::PgPool,
    windows: &[ResolvedSeasonWindow],
    target_window: &ResolvedSeasonWindow,
    generated_at: DateTime<Utc>,
    override_points: &[SeasonScalarOverridePoint],
) -> Result<Option<ScalarProjection>, String> {
    let season_ids: Vec<i32> = windows.iter().map(|window| window.season_id).collect();
    if season_ids.is_empty() {
        return Ok(None);
    }

    let rows: Vec<ScalarSampleRow> = sqlx::query_as(
        "SELECT season_id, sampled_at, scalar_weighted, confidence, sample_count \
         FROM season_scalar_samples \
         WHERE season_id = ANY($1) \
         ORDER BY season_id ASC, sampled_at ASC",
    )
    .bind(&season_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load season scalar samples: {e}"))?;

    let curves = build_curves(windows, &rows, override_points);
    let Some(target_curve) = curves.get(&target_window.season_id) else {
        return Ok(None);
    };
    let Some(current_point) = current_scalar_point(target_curve, target_window, generated_at)
    else {
        return Ok(None);
    };
    let current_progress =
        progress_ratio(target_window.start_at, target_window.end_at, generated_at);

    let historical_priors =
        build_historical_step_priors(windows, &curves, target_window.season_id, generated_at);
    let future_points = infer_future_step_points(
        target_window,
        generated_at,
        current_progress,
        current_point.scalar_weighted,
        &historical_priors,
        override_points,
    );

    let mut points = Vec::with_capacity(future_points.len() + 1);
    points.push(ScalarPointInternal {
        sampled_at: generated_at,
        scalar_weighted: current_point.scalar_weighted,
        source: current_point.source,
    });
    points.extend(future_points);
    deduplicate_scalar_points(&mut points);

    Ok(Some(ScalarProjection {
        current_scalar_weighted: current_point.scalar_weighted,
        points,
    }))
}

pub fn momentum_half_life_hours() -> f64 {
    MOMENTUM_HALF_LIFE_HOURS
}

pub fn project_momentum_gain(excess_rate_per_hour: f64, remaining_hours: f64) -> i64 {
    if !excess_rate_per_hour.is_finite() || excess_rate_per_hour <= 0.0 || remaining_hours <= 0.0 {
        return 0;
    }
    let lambda = std::f64::consts::LN_2 / MOMENTUM_HALF_LIFE_HOURS;
    let gain = excess_rate_per_hour * ((1.0 - (-lambda * remaining_hours).exp()) / lambda);
    gain.round().max(0.0) as i64
}

impl ScalarProjection {
    pub fn current_scalar_weighted(&self) -> f64 {
        self.current_scalar_weighted
    }

    pub fn uses_estimated_points(&self) -> bool {
        self.points
            .iter()
            .skip(1)
            .any(|point| point.source == ScalarPointSource::Estimated)
    }

    pub fn uses_manual_override_points(&self) -> bool {
        self.points
            .iter()
            .skip(1)
            .any(|point| point.source == ScalarPointSource::ManualOverride)
    }

    pub fn api_points(&self) -> Vec<SeasonScalarPoint> {
        self.points
            .iter()
            .map(|point| SeasonScalarPoint {
                sampled_at: point.sampled_at.to_rfc3339(),
                scalar_weighted: point.scalar_weighted,
                source: point.source,
            })
            .collect()
    }

    pub fn projected_passive_gain(
        &self,
        territory_count: usize,
        generated_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
    ) -> i64 {
        if territory_count == 0 || generated_at >= end_at {
            return 0;
        }

        let mut total_gain = 0.0;
        let mut current_at = generated_at;
        let mut current_scalar = self.current_scalar_weighted;

        for point in self.points.iter().skip(1) {
            let segment_end = point.sampled_at.min(end_at);
            if segment_end > current_at {
                let hours = (segment_end - current_at).num_seconds() as f64 / 3600.0;
                total_gain += passive_sr_per_hour(territory_count, current_scalar) * hours;
            }
            current_at = segment_end;
            current_scalar = point.scalar_weighted;
            if current_at >= end_at {
                break;
            }
        }

        if current_at < end_at {
            let hours = (end_at - current_at).num_seconds() as f64 / 3600.0;
            total_gain += passive_sr_per_hour(territory_count, current_scalar) * hours;
        }

        total_gain.round().max(0.0) as i64
    }
}

fn build_curves(
    windows: &[ResolvedSeasonWindow],
    rows: &[ScalarSampleRow],
    override_points: &[SeasonScalarOverridePoint],
) -> HashMap<i32, SeasonScalarCurve> {
    let windows_by_season: HashMap<i32, &ResolvedSeasonWindow> = windows
        .iter()
        .map(|window| (window.season_id, window))
        .collect();
    let mut raw_buckets: HashMap<i32, Vec<Vec<(f64, f64)>>> = HashMap::new();
    let mut latest_by_season: HashMap<i32, ScalarPointInternal> = HashMap::new();

    for (season_id, sampled_at, scalar_weighted, confidence, sample_count) in rows {
        let Some(window) = windows_by_season.get(season_id) else {
            continue;
        };
        if !scalar_weighted.is_finite() || *scalar_weighted <= 0.0 {
            continue;
        }
        if *sampled_at < window.start_at || *sampled_at > window.end_at {
            continue;
        }
        let bucket = bucket_index(progress_ratio(window.start_at, window.end_at, *sampled_at));
        let weight = sample_weight(*confidence, *sample_count);
        raw_buckets
            .entry(*season_id)
            .or_insert_with(|| vec![Vec::new(); PROGRESS_BUCKETS])[bucket]
            .push((*scalar_weighted, weight));
        latest_by_season
            .entry(*season_id)
            .and_modify(|latest| {
                if *sampled_at >= latest.sampled_at {
                    *latest = ScalarPointInternal {
                        sampled_at: *sampled_at,
                        scalar_weighted: *scalar_weighted,
                        source: ScalarPointSource::Observed,
                    };
                }
            })
            .or_insert_with(|| ScalarPointInternal {
                sampled_at: *sampled_at,
                scalar_weighted: *scalar_weighted,
                source: ScalarPointSource::Observed,
            });
    }

    for point in override_points {
        let Some(window) = windows_by_season.get(&point.season_id) else {
            continue;
        };
        if point.starts_at < window.start_at || point.starts_at > window.end_at {
            continue;
        }
        let bucket = bucket_index(progress_ratio(
            window.start_at,
            window.end_at,
            point.starts_at,
        ));
        raw_buckets
            .entry(point.season_id)
            .or_insert_with(|| vec![Vec::new(); PROGRESS_BUCKETS])[bucket]
            .push((point.scalar_weighted, 10_000.0));
        latest_by_season
            .entry(point.season_id)
            .and_modify(|latest| {
                if point.starts_at >= latest.sampled_at {
                    *latest = ScalarPointInternal {
                        sampled_at: point.starts_at,
                        scalar_weighted: point.scalar_weighted,
                        source: ScalarPointSource::ManualOverride,
                    };
                }
            })
            .or_insert_with(|| ScalarPointInternal {
                sampled_at: point.starts_at,
                scalar_weighted: point.scalar_weighted,
                source: ScalarPointSource::ManualOverride,
            });
    }

    let mut curves = HashMap::new();
    for window in windows {
        let mut buckets = raw_buckets
            .remove(&window.season_id)
            .unwrap_or_else(|| vec![Vec::new(); PROGRESS_BUCKETS]);
        let mut current_max: Option<f64> = None;
        let mut bucket_values = Vec::with_capacity(PROGRESS_BUCKETS);

        for values in &mut buckets {
            let bucket_value = weighted_median(values)
                .map(|value| current_max.map_or(value, |max_value| value.max(max_value)));
            if let Some(value) = bucket_value {
                current_max = Some(value);
            }
            bucket_values.push(current_max);
        }

        curves.insert(
            window.season_id,
            SeasonScalarCurve {
                latest: latest_by_season.get(&window.season_id).cloned(),
                bucket_values,
            },
        );
    }
    curves
}

fn build_historical_step_priors(
    windows: &[ResolvedSeasonWindow],
    curves: &HashMap<i32, SeasonScalarCurve>,
    target_season_id: i32,
    generated_at: DateTime<Utc>,
) -> Vec<Option<f64>> {
    let mut priors = Vec::with_capacity(SCALAR_LEVELS.len());
    for level in SCALAR_LEVELS {
        let mut ratios = Vec::new();
        for window in windows {
            if window.season_id == target_season_id || window.end_at > generated_at {
                continue;
            }
            let Some(curve) = curves.get(&window.season_id) else {
                continue;
            };
            if let Some(progress) = first_progress_for_level(curve, level) {
                ratios.push(progress);
            }
        }
        priors.push(median(&mut ratios));
    }
    priors
}

fn current_scalar_point(
    curve: &SeasonScalarCurve,
    _window: &ResolvedSeasonWindow,
    generated_at: DateTime<Utc>,
) -> Option<ScalarPointInternal> {
    curve.latest.as_ref().and_then(|latest| {
        if latest.sampled_at <= generated_at {
            Some(latest.clone())
        } else {
            None
        }
    })
}

fn infer_future_step_points(
    window: &ResolvedSeasonWindow,
    generated_at: DateTime<Utc>,
    current_progress: f64,
    current_scalar: f64,
    historical_priors: &[Option<f64>],
    override_points: &[SeasonScalarOverridePoint],
) -> Vec<ScalarPointInternal> {
    let current_level_index = SCALAR_LEVELS
        .iter()
        .rposition(|level| current_scalar + LEVEL_EPSILON >= *level)
        .unwrap_or(0);
    let future_levels: Vec<f64> = SCALAR_LEVELS
        .iter()
        .copied()
        .skip(current_level_index + 1)
        .collect();
    let manual_by_level: HashMap<i64, &SeasonScalarOverridePoint> = override_points
        .iter()
        .filter(|point| point.season_id == window.season_id && point.starts_at > generated_at)
        .map(|point| (scalar_level_key(point.scalar_weighted), point))
        .collect();

    let mut points = Vec::new();
    let mut cursor_progress = current_progress;
    let total_future_steps = future_levels.len();
    for (idx, level) in future_levels.into_iter().enumerate() {
        if let Some(override_point) = manual_by_level.get(&scalar_level_key(level)) {
            let progress = progress_ratio(window.start_at, window.end_at, override_point.starts_at)
                .max(cursor_progress);
            cursor_progress = progress;
            points.push(ScalarPointInternal {
                sampled_at: override_point.starts_at,
                scalar_weighted: level,
                source: ScalarPointSource::ManualOverride,
            });
            continue;
        }

        let historical_progress = historical_priors
            .get(current_level_index + 1 + idx)
            .copied()
            .flatten()
            .filter(|progress| *progress > cursor_progress);
        let remaining_levels = total_future_steps - idx;
        let fallback_progress =
            cursor_progress + ((1.0 - cursor_progress) / (remaining_levels as f64 + 1.0));
        let next_progress = historical_progress
            .unwrap_or(fallback_progress)
            .clamp(0.0, 1.0);
        cursor_progress = next_progress;
        points.push(ScalarPointInternal {
            sampled_at: time_at_progress(window.start_at, window.end_at, next_progress),
            scalar_weighted: level,
            source: ScalarPointSource::Estimated,
        });
    }

    points
}

fn deduplicate_scalar_points(points: &mut Vec<ScalarPointInternal>) {
    points.sort_by(|left, right| left.sampled_at.cmp(&right.sampled_at));
    let mut deduped: Vec<ScalarPointInternal> = Vec::with_capacity(points.len());
    for point in points.iter() {
        if let Some(last) = deduped.last_mut()
            && point.sampled_at == last.sampled_at
        {
            if point.scalar_weighted >= last.scalar_weighted {
                *last = point.clone();
            }
            continue;
        }
        deduped.push(point.clone());
    }
    *points = deduped;
}

fn weighted_median(values: &mut Vec<(f64, f64)>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.0.partial_cmp(&right.0).unwrap_or(Ordering::Equal));
    let total_weight: f64 = values.iter().map(|(_, weight)| *weight).sum();
    if total_weight <= 0.0 {
        return Some(values[values.len() / 2].0);
    }
    let mut cumulative = 0.0;
    let midpoint = total_weight / 2.0;
    for (value, weight) in values.iter() {
        cumulative += *weight;
        if cumulative >= midpoint {
            return Some(*value);
        }
    }
    values.last().map(|(value, _)| *value)
}

fn median(values: &mut Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let midpoint = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[midpoint - 1] + values[midpoint]) / 2.0)
    } else {
        Some(values[midpoint])
    }
}

fn sample_weight(confidence: f64, sample_count: i32) -> f64 {
    let normalized_confidence = if confidence.is_finite() {
        confidence.max(MIN_SAMPLE_WEIGHT)
    } else {
        MIN_SAMPLE_WEIGHT
    };
    let normalized_count = sample_count.max(1) as f64;
    normalized_confidence * normalized_count
}

fn first_progress_for_level(curve: &SeasonScalarCurve, level: f64) -> Option<f64> {
    curve
        .bucket_values
        .iter()
        .enumerate()
        .find_map(|(idx, value)| {
            value.and_then(|scalar| {
                if scalar + LEVEL_EPSILON >= level {
                    Some(idx as f64 / (PROGRESS_BUCKETS - 1) as f64)
                } else {
                    None
                }
            })
        })
}

fn progress_ratio(start_at: DateTime<Utc>, end_at: DateTime<Utc>, at: DateTime<Utc>) -> f64 {
    let total_seconds = (end_at - start_at).num_seconds().max(1) as f64;
    let elapsed_seconds = (at - start_at)
        .num_seconds()
        .clamp(0, (end_at - start_at).num_seconds()) as f64;
    (elapsed_seconds / total_seconds).clamp(0.0, 1.0)
}

fn time_at_progress(
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    progress: f64,
) -> DateTime<Utc> {
    let total_seconds = (end_at - start_at).num_seconds().max(1) as f64;
    let elapsed_seconds = (total_seconds * progress.clamp(0.0, 1.0)).round() as i64;
    start_at + Duration::seconds(elapsed_seconds)
}

fn bucket_index(progress: f64) -> usize {
    let progress = progress.clamp(0.0, 1.0);
    ((progress * (PROGRESS_BUCKETS.saturating_sub(1) as f64)).floor() as usize)
        .min(PROGRESS_BUCKETS.saturating_sub(1))
}

fn scalar_level_key(value: f64) -> i64 {
    (value * 1000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        LEVEL_EPSILON, MOMENTUM_HALF_LIFE_HOURS, PROGRESS_BUCKETS, ScalarPointInternal,
        ScalarPointSource, ScalarProjection, SeasonScalarCurve, bucket_index,
        build_historical_step_priors, infer_future_step_points, progress_ratio,
        project_momentum_gain, sample_weight, time_at_progress, weighted_median,
    };
    use crate::config::SeasonScalarOverridePoint;
    use crate::services::season_data::{ResolvedSeasonWindow, SeasonWindowSource};
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;

    fn ts(value: &str) -> DateTime<Utc> {
        value.parse::<DateTime<Utc>>().expect("parse timestamp")
    }

    fn season_window(season_id: i32, start: &str, end: &str) -> ResolvedSeasonWindow {
        ResolvedSeasonWindow {
            season_id,
            label: Some(format!("Season {season_id}")),
            start_at: ts(start),
            end_at: ts(end),
            source: SeasonWindowSource::Configured,
        }
    }

    #[test]
    fn weighted_median_prefers_cluster_over_outlier() {
        let mut values = vec![(1.0, 1.0), (1.1, 4.0), (10.0, 0.1)];
        let median = weighted_median(&mut values).expect("median");
        assert!((median - 1.1).abs() < LEVEL_EPSILON);
    }

    #[test]
    fn infer_future_steps_uses_manual_override_before_estimate() {
        let window = season_window(30, "2026-03-26T00:00:00Z", "2026-04-22T00:00:00Z");
        let points = infer_future_step_points(
            &window,
            ts("2026-03-27T00:00:00Z"),
            0.05,
            1.0,
            &[None, Some(0.1), Some(0.2), None, None, None],
            &[SeasonScalarOverridePoint {
                season_id: 30,
                starts_at: ts("2026-04-01T00:00:00Z"),
                scalar_weighted: 1.5,
            }],
        );

        assert_eq!(
            points.first().expect("first point").source,
            ScalarPointSource::ManualOverride
        );
        assert_eq!(
            points.first().expect("first point").sampled_at,
            ts("2026-04-01T00:00:00Z")
        );
    }

    #[test]
    fn build_historical_step_priors_returns_median_progresses() {
        let window_a = season_window(29, "2026-02-27T00:00:00Z", "2026-03-27T00:00:00Z");
        let window_b = season_window(28, "2026-01-27T00:00:00Z", "2026-02-27T00:00:00Z");
        let mut curves = HashMap::new();
        let mut curve_a = SeasonScalarCurve::default();
        curve_a.bucket_values = vec![Some(1.0); PROGRESS_BUCKETS];
        curve_a.bucket_values[bucket_index(0.2)] = Some(1.5);
        curve_a.bucket_values[bucket_index(0.6)] = Some(3.0);
        let mut curve_b = SeasonScalarCurve::default();
        curve_b.bucket_values = vec![Some(1.0); PROGRESS_BUCKETS];
        curve_b.bucket_values[bucket_index(0.3)] = Some(1.5);
        curve_b.bucket_values[bucket_index(0.8)] = Some(3.0);
        curves.insert(29, curve_a);
        curves.insert(28, curve_b);

        let priors = build_historical_step_priors(
            &[window_a, window_b],
            &curves,
            30,
            ts("2026-04-01T00:00:00Z"),
        );

        assert!(priors[1].expect("1.5 progress") > 0.2);
        assert!(priors[3].expect("3.0 progress") > 0.6);
    }

    #[test]
    fn scalar_projection_passive_gain_respects_future_steps() {
        let projection = ScalarProjection {
            current_scalar_weighted: 1.0,
            points: vec![
                ScalarPointInternal {
                    sampled_at: ts("2026-03-27T00:00:00Z"),
                    scalar_weighted: 1.0,
                    source: ScalarPointSource::Observed,
                },
                ScalarPointInternal {
                    sampled_at: ts("2026-03-28T00:00:00Z"),
                    scalar_weighted: 2.0,
                    source: ScalarPointSource::Estimated,
                },
            ],
        };

        let gain = projection.projected_passive_gain(
            5,
            ts("2026-03-27T00:00:00Z"),
            ts("2026-03-29T00:00:00Z"),
        );

        assert!(gain > 0);
    }

    #[test]
    fn momentum_gain_decays_over_time() {
        let gain = project_momentum_gain(100.0, MOMENTUM_HALF_LIFE_HOURS);
        assert!(gain > 0);
        assert!(gain < (100.0 * MOMENTUM_HALF_LIFE_HOURS) as i64);
    }

    #[test]
    fn sample_weight_respects_minimums() {
        assert!(sample_weight(0.0, 0) >= 0.05);
    }

    #[test]
    fn progress_ratio_and_time_at_progress_round_trip() {
        let start = ts("2026-03-26T00:00:00Z");
        let end = ts("2026-04-22T00:00:00Z");
        let mid = ts("2026-04-08T12:00:00Z");
        let progress = progress_ratio(start, end, mid);
        let round_trip = time_at_progress(start, end, progress);
        assert_eq!(round_trip, mid);
    }
}
