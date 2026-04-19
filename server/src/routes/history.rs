use std::cmp::Ordering;
use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use sequoia_shared::SeasonScalarSample;
use sequoia_shared::history::{
    HistoryBounds, HistoryEvent, HistoryEvents, HistoryGuildSrEntry, HistoryHeat, HistoryHeatEntry,
    HistoryHeatMeta, HistoryHeatSeasonWindow, HistoryHeatSource, HistorySnapshot, HistorySrSamples,
    HistorySrSnapshot, OwnershipRecord,
};
use serde::Deserialize;

use crate::config::territory_history_retention_days;
use crate::state::{AppState, build_guild_color_lookup, lookup_guild_color};

type HistoryEventRow = (
    i64,
    DateTime<Utc>,
    DateTime<Utc>,
    String,
    String,
    String,
    String,
    Option<i16>,
    Option<i16>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<i16>,
    Option<i16>,
    Option<i16>,
);
type HistoryBoundsRow = (
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i64,
    Option<i64>,
);
type SeasonScalarRow = (DateTime<Utc>, i32, f64, f64, f64, i32);
const AUTHORITATIVE_SCALAR_CONFIDENCE_MIN: f64 = 0.99;
const AUTHORITATIVE_SCALAR_SAMPLE_COUNT_MIN: i32 = 1;
type SeasonObservationRow = (
    DateTime<Utc>,
    i32,
    String,
    String,
    String,
    i16,
    i32,
    Option<i32>,
    Option<i32>,
);
type SeasonWindowRow = (i32, DateTime<Utc>, DateTime<Utc>);
type HeatCountRow = (String, i64);

const HEAT_SEASON_FALLBACK_DAYS: i64 = 60;

#[derive(Debug, Clone)]
struct SeasonObservation {
    observed_at: DateTime<Utc>,
    season_id: i32,
    guild_name: String,
    guild_uuid: String,
    guild_prefix: String,
    territory_count: i16,
    season_rating: i32,
    sr_gain_5m: Option<i32>,
    sample_rank: Option<i32>,
}

impl From<SeasonObservationRow> for SeasonObservation {
    fn from(value: SeasonObservationRow) -> Self {
        let (
            observed_at,
            season_id,
            guild_name,
            guild_uuid,
            guild_prefix,
            territory_count,
            season_rating,
            sr_gain_5m,
            sample_rank,
        ) = value;
        Self {
            observed_at,
            season_id,
            guild_name,
            guild_uuid,
            guild_prefix,
            territory_count,
            season_rating,
            sr_gain_5m,
            sample_rank,
        }
    }
}

fn parse_rgb_triplet(r: Option<i16>, g: Option<i16>, b: Option<i16>) -> Option<(u8, u8, u8)> {
    match (r, g, b) {
        (Some(r), Some(g), Some(b)) => Some((
            u8::try_from(r).ok()?,
            u8::try_from(g).ok()?,
            u8::try_from(b).ok()?,
        )),
        _ => None,
    }
}

fn with_fallback_color(
    color: Option<(u8, u8, u8)>,
    guild_name: &str,
    fallback_colors: &HashMap<String, (u8, u8, u8)>,
    fallback_colors_normalized: &HashMap<String, (u8, u8, u8)>,
) -> Option<(u8, u8, u8)> {
    color.or_else(|| lookup_guild_color(fallback_colors, fallback_colors_normalized, guild_name))
}

async fn merged_fallback_colors(
    state: &AppState,
    pool: &sqlx::PgPool,
) -> Result<(HashMap<String, (u8, u8, u8)>, HashMap<String, (u8, u8, u8)>), StatusCode> {
    let mut fallback_colors = state.guild_colors.read().await.clone();
    let persisted_rows: Vec<(String, i16, i16, i16)> =
        sqlx::query_as("SELECT guild_name, color_r, color_g, color_b FROM guild_color_cache")
            .fetch_all(pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for (guild_name, color_r, color_g, color_b) in persisted_rows {
        let Some(color) = parse_rgb_triplet(Some(color_r), Some(color_g), Some(color_b)) else {
            continue;
        };
        // Prefer in-memory live Athena colors when present.
        fallback_colors.entry(guild_name).or_insert(color);
    }

    let fallback_colors_normalized = build_guild_color_lookup(&fallback_colors);
    Ok((fallback_colors, fallback_colors_normalized))
}

fn cmp_by_rating_then_territories_then_name(
    a: &SeasonObservation,
    b: &SeasonObservation,
) -> Ordering {
    b.season_rating
        .cmp(&a.season_rating)
        .then_with(|| b.territory_count.cmp(&a.territory_count))
        .then_with(|| a.guild_name.cmp(&b.guild_name))
}

fn build_sr_entries(
    mut rows: Vec<SeasonObservation>,
    prefer_sample_rank: bool,
) -> Vec<HistoryGuildSrEntry> {
    if rows.is_empty() {
        return Vec::new();
    }

    if prefer_sample_rank {
        rows.sort_by(|a, b| {
            a.sample_rank
                .unwrap_or(i32::MAX)
                .cmp(&b.sample_rank.unwrap_or(i32::MAX))
                .then_with(|| cmp_by_rating_then_territories_then_name(a, b))
        });
    } else {
        rows.sort_by(cmp_by_rating_then_territories_then_name);
    }

    rows.into_iter()
        .enumerate()
        .map(|(idx, row)| {
            let fallback_rank = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            let season_rank = if prefer_sample_rank {
                row.sample_rank
                    .and_then(|rank| u32::try_from(rank).ok())
                    .filter(|rank| *rank > 0)
                    .unwrap_or(fallback_rank)
            } else {
                fallback_rank
            };

            HistoryGuildSrEntry {
                guild_uuid: row.guild_uuid,
                guild_name: row.guild_name,
                guild_prefix: row.guild_prefix,
                sampled_at: row.observed_at.to_rfc3339(),
                season_id: row.season_id,
                season_rating: i64::from(row.season_rating),
                season_rank,
                sr_gain_5m: row.sr_gain_5m.map(i64::from),
            }
        })
        .collect()
}

fn build_sr_samples(rows: Vec<SeasonObservation>) -> Vec<HistorySrSnapshot> {
    if rows.is_empty() {
        return Vec::new();
    }

    let mut grouped: Vec<HistorySrSnapshot> = Vec::new();
    let mut current_at: Option<DateTime<Utc>> = None;
    let mut current_rows: Vec<SeasonObservation> = Vec::new();

    for row in rows {
        match current_at {
            Some(observed_at) if observed_at == row.observed_at => {
                current_rows.push(row);
            }
            Some(observed_at) => {
                grouped.push(HistorySrSnapshot {
                    sampled_at: observed_at.to_rfc3339(),
                    entries: build_sr_entries(std::mem::take(&mut current_rows), true),
                });
                current_at = Some(row.observed_at);
                current_rows.push(row);
            }
            None => {
                current_at = Some(row.observed_at);
                current_rows.push(row);
            }
        }
    }

    if let Some(observed_at) = current_at {
        grouped.push(HistorySrSnapshot {
            sampled_at: observed_at.to_rfc3339(),
            entries: build_sr_entries(current_rows, true),
        });
    }

    grouped
}

async fn season_leaderboard_at(
    pool: &sqlx::PgPool,
    guild_names: &[String],
    target: DateTime<Utc>,
) -> Result<Option<Vec<HistoryGuildSrEntry>>, StatusCode> {
    if guild_names.is_empty() {
        return Ok(None);
    }

    let rows: Vec<SeasonObservationRow> = sqlx::query_as(
        "SELECT observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, \
                season_rating, sr_gain_5m, sample_rank \
         FROM ( \
             SELECT DISTINCT ON (guild_name) observed_at, season_id, guild_name, guild_uuid, \
                    guild_prefix, territory_count, season_rating, sr_gain_5m, sample_rank \
             FROM season_guild_observations \
             WHERE observed_at <= $1 AND guild_name = ANY($2) \
             ORDER BY guild_name, observed_at DESC \
         ) latest",
    )
    .bind(target)
    .bind(guild_names)
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows.is_empty() {
        return Ok(None);
    }

    let observations: Vec<SeasonObservation> =
        rows.into_iter().map(SeasonObservation::from).collect();
    Ok(Some(build_sr_entries(observations, false)))
}

#[derive(Deserialize)]
pub struct AtQuery {
    t: String,
}

#[derive(Deserialize)]
pub struct EventsQuery {
    from: String,
    to: String,
    #[serde(default)]
    after_seq: Option<u64>,
    #[serde(default = "default_limit")]
    limit: i64,
}

#[derive(Deserialize)]
pub struct SrSamplesQuery {
    from: String,
    to: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum HeatQuerySource {
    Season,
    AllTime,
}

#[derive(Deserialize)]
pub struct HeatQuery {
    source: HeatQuerySource,
    #[serde(default)]
    season_id: Option<i32>,
    #[serde(default)]
    at: Option<String>,
}

#[derive(Debug, Clone)]
struct SeasonWindow {
    season_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

fn default_limit() -> i64 {
    500
}

fn parse_time_window(from: &str, to: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), StatusCode> {
    let from = from
        .parse::<DateTime<Utc>>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let to = to
        .parse::<DateTime<Utc>>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((from, to))
}

fn parse_optional_timestamp(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, StatusCode> {
    raw.map(|value| {
        value
            .parse::<DateTime<Utc>>()
            .map_err(|_| StatusCode::BAD_REQUEST)
    })
    .transpose()
}

async fn load_season_windows(pool: &sqlx::PgPool) -> Result<Vec<SeasonWindow>, StatusCode> {
    let rows: Vec<SeasonWindowRow> = sqlx::query_as(
        "SELECT season_id, MIN(observed_at), MAX(observed_at) \
         FROM season_guild_observations \
         GROUP BY season_id \
         ORDER BY season_id DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(rows
        .into_iter()
        .map(|(season_id, start, end)| SeasonWindow {
            season_id,
            start,
            end,
        })
        .collect())
}

async fn load_owner_change_earliest(
    pool: &sqlx::PgPool,
) -> Result<Option<DateTime<Utc>>, StatusCode> {
    sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT MIN(recorded_at) \
         FROM territory_events \
         WHERE prev_guild_uuid IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// `GET /api/history/at?t={rfc3339}` — Reconstruct ownership at a point in time.
pub async fn history_at(
    State(state): State<AppState>,
    Query(query): Query<AtQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let max_replay_events = state.max_history_replay_events;

    let target: DateTime<Utc> = query
        .t
        .parse::<DateTime<Utc>>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let (fallback_colors, fallback_colors_normalized) =
        merged_fallback_colors(&state, pool).await?;

    let season_scalar_fut = async {
        sqlx::query_as::<_, SeasonScalarRow>(
            "SELECT sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count \
             FROM season_scalar_samples \
             WHERE sampled_at <= $1 \
             ORDER BY (confidence >= $2 AND sample_count >= $3) DESC, sampled_at DESC \
             LIMIT 1",
        )
        .bind(target)
        .bind(AUTHORITATIVE_SCALAR_CONFIDENCE_MIN)
        .bind(AUTHORITATIVE_SCALAR_SAMPLE_COUNT_MIN)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .map(|row| {
            row.map(
                |(sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count)| {
                    SeasonScalarSample {
                        sampled_at: sampled_at.to_rfc3339(),
                        season_id,
                        scalar_weighted,
                        scalar_raw,
                        confidence,
                        sample_count: u32::try_from(sample_count.max(0)).unwrap_or(u32::MAX),
                    }
                },
            )
        })
    };
    let snapshot_fut = async {
        sqlx::query_as::<_, (i64, DateTime<Utc>, serde_json::Value)>(
            "SELECT id, created_at, ownership FROM territory_snapshots \
             WHERE created_at <= $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(target)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    };
    let events_fut = async {
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                Option<i16>,
                Option<i16>,
                Option<i16>,
                DateTime<Utc>,
            ),
        >(
            "SELECT territory, guild_uuid, guild_name, guild_prefix, \
                    guild_color_r, guild_color_g, guild_color_b, acquired_at \
             FROM territory_events \
             WHERE recorded_at > COALESCE( \
                   (SELECT created_at FROM territory_snapshots WHERE created_at <= $1 \
                    ORDER BY created_at DESC LIMIT 1), \
                   '1970-01-01T00:00:00Z'::timestamptz \
                 ) \
               AND recorded_at <= $2 \
             ORDER BY stream_seq ASC \
             LIMIT $3",
        )
        .bind(target)
        .bind(target)
        .bind(max_replay_events.saturating_add(1))
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    };
    let (season_scalar, snapshot_row, event_rows) =
        tokio::try_join!(season_scalar_fut, snapshot_fut, events_fut)?;

    if event_rows.len() as i64 > max_replay_events {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let mut ownership = match snapshot_row {
        Some((_id, _created_at, ownership_json)) => {
            let ownership: HashMap<String, OwnershipRecord> =
                serde_json::from_value(ownership_json)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            ownership
        }
        None => HashMap::new(),
    };

    for (
        territory,
        guild_uuid,
        guild_name,
        guild_prefix,
        guild_color_r,
        guild_color_g,
        guild_color_b,
        acquired_at,
    ) in event_rows
    {
        let guild_color = with_fallback_color(
            parse_rgb_triplet(guild_color_r, guild_color_g, guild_color_b),
            &guild_name,
            &fallback_colors,
            &fallback_colors_normalized,
        );
        ownership.insert(
            territory,
            OwnershipRecord {
                guild_uuid,
                guild_name,
                guild_prefix,
                guild_color,
                acquired_at: acquired_at.to_rfc3339(),
            },
        );
    }

    for record in ownership.values_mut() {
        record.guild_color = with_fallback_color(
            record.guild_color,
            &record.guild_name,
            &fallback_colors,
            &fallback_colors_normalized,
        );
    }

    let mut guild_names: Vec<String> = ownership
        .values()
        .map(|record| record.guild_name.clone())
        .collect();
    guild_names.sort();
    guild_names.dedup();
    let season_leaderboard = season_leaderboard_at(pool, &guild_names, target).await?;

    let snapshot = HistorySnapshot {
        timestamp: target.to_rfc3339(),
        ownership,
        season_scalar,
        season_leaderboard,
    };

    // Cache older timestamps aggressively, recent ones briefly
    let age_secs = (Utc::now() - target).num_seconds();
    let max_age = if age_secs > 3600 { 86400 } else { 60 };

    let mut headers = HeaderMap::new();
    let cache_control = HeaderValue::from_str(&format!("public, max-age={max_age}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    headers.insert(header::CACHE_CONTROL, cache_control);

    Ok((headers, Json(snapshot)))
}

/// `GET /api/history/events?from={t}&to={t}&limit={n}&after_seq={seq}` — Paginated event list.
pub async fn history_events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let (from, to) = parse_time_window(&query.from, &query.to)?;
    let limit = query.limit.clamp(1, 1000);
    let after_seq = match query.after_seq {
        Some(seq) => Some(i64::try_from(seq).map_err(|_| StatusCode::BAD_REQUEST)?),
        None => None,
    };
    let (fallback_colors, fallback_colors_normalized) =
        merged_fallback_colors(&state, pool).await?;

    let rows: Vec<HistoryEventRow> = if let Some(after_seq) = after_seq {
        sqlx::query_as(
            "SELECT stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, \
                    guild_prefix, guild_color_r, guild_color_g, guild_color_b, \
                    prev_guild_name, prev_guild_prefix, \
                    prev_guild_color_r, prev_guild_color_g, prev_guild_color_b \
             FROM territory_events \
             WHERE stream_seq > $1 AND recorded_at > $2 AND recorded_at <= $3 \
             ORDER BY stream_seq ASC \
             LIMIT $4",
        )
        .bind(after_seq)
        .bind(from)
        .bind(to)
        .bind(limit + 1)
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        sqlx::query_as(
            "SELECT stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, \
                    guild_prefix, guild_color_r, guild_color_g, guild_color_b, \
                    prev_guild_name, prev_guild_prefix, \
                    prev_guild_color_r, prev_guild_color_g, prev_guild_color_b \
             FROM territory_events \
             WHERE recorded_at > $1 AND recorded_at <= $2 \
             ORDER BY stream_seq ASC \
             LIMIT $3",
        )
        .bind(from)
        .bind(to)
        .bind(limit + 1)
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    let has_more = rows.len() as i64 > limit;
    let mut events = Vec::with_capacity(limit as usize);
    for (
        stream_seq,
        recorded_at,
        acquired_at,
        territory,
        guild_uuid,
        guild_name,
        guild_prefix,
        guild_color_r,
        guild_color_g,
        guild_color_b,
        prev_guild_name,
        prev_guild_prefix,
        prev_guild_color_r,
        prev_guild_color_g,
        prev_guild_color_b,
    ) in rows.into_iter().take(limit as usize)
    {
        let guild_color = with_fallback_color(
            parse_rgb_triplet(guild_color_r, guild_color_g, guild_color_b),
            &guild_name,
            &fallback_colors,
            &fallback_colors_normalized,
        );
        let prev_guild_color = prev_guild_name.as_deref().and_then(|name| {
            with_fallback_color(
                parse_rgb_triplet(prev_guild_color_r, prev_guild_color_g, prev_guild_color_b),
                name,
                &fallback_colors,
                &fallback_colors_normalized,
            )
        });

        events.push(HistoryEvent {
            stream_seq: u64::try_from(stream_seq).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            timestamp: recorded_at.to_rfc3339(),
            acquired_at: Some(acquired_at.to_rfc3339()),
            territory,
            guild_uuid,
            guild_name,
            guild_prefix,
            guild_color,
            prev_guild_name,
            prev_guild_prefix,
            prev_guild_color,
        });
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );

    Ok((headers, Json(HistoryEvents { events, has_more })))
}

/// `GET /api/history/sr-samples?from={t}&to={t}` — Season rating snapshots over a time window.
pub async fn history_sr_samples(
    State(state): State<AppState>,
    Query(query): Query<SrSamplesQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let (from, to) = parse_time_window(&query.from, &query.to)?;
    let max_sample_rows = state.max_history_sr_sample_rows;

    let season_sr_rows: Vec<SeasonObservationRow> = sqlx::query_as::<_, SeasonObservationRow>(
        "SELECT observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, \
                season_rating, sr_gain_5m, sample_rank \
         FROM season_guild_observations \
         WHERE observed_at > $1 AND observed_at <= $2 \
         ORDER BY observed_at ASC, sample_rank ASC NULLS LAST, season_rating DESC, \
                  territory_count DESC, guild_name ASC \
         LIMIT $3",
    )
    .bind(from)
    .bind(to)
    .bind(max_sample_rows.saturating_add(1))
    .fetch_all(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if season_sr_rows.len() as i64 > max_sample_rows {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let samples = build_sr_samples(
        season_sr_rows
            .into_iter()
            .map(SeasonObservation::from)
            .collect(),
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );

    Ok((headers, Json(HistorySrSamples { samples })))
}

/// `GET /api/history/bounds` — Returns earliest/latest timestamps and event count.
pub async fn history_bounds(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let row: HistoryBoundsRow = sqlx::query_as(
        "SELECT MIN(recorded_at), MAX(recorded_at), COUNT(*), MAX(stream_seq) FROM territory_events",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let bounds = HistoryBounds {
        earliest: row.0.map(|dt| dt.to_rfc3339()),
        latest: row.1.map(|dt| dt.to_rfc3339()),
        event_count: row.2,
        latest_seq: row.3.and_then(|v| u64::try_from(v).ok()),
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=30"),
    );

    Ok((headers, Json(bounds)))
}

/// `GET /api/history/heat/meta` — Heat-map metadata for season selection and window hints.
pub async fn history_heat_meta(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let season_windows = load_season_windows(pool).await?;
    let latest_season_id = season_windows.first().map(|window| window.season_id);
    let seasons = season_windows
        .into_iter()
        .map(|window| HistoryHeatSeasonWindow {
            season_id: window.season_id,
            start: window.start.to_rfc3339(),
            end: window.end.to_rfc3339(),
            is_current: Some(window.season_id) == latest_season_id,
        })
        .collect();
    let all_time_earliest = load_owner_change_earliest(pool)
        .await?
        .map(|dt| dt.to_rfc3339());
    let meta = HistoryHeatMeta {
        latest_season_id,
        seasons,
        all_time_earliest,
        retention_days: territory_history_retention_days(),
        season_fallback_days: HEAT_SEASON_FALLBACK_DAYS,
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );

    Ok((headers, Json(meta)))
}

/// `GET /api/history/heat` — Territory takeover counts for the selected heat-map window.
pub async fn history_heat(
    State(state): State<AppState>,
    Query(query): Query<HeatQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let now = Utc::now();
    let at = parse_optional_timestamp(query.at.as_deref())?;
    let season_windows = load_season_windows(pool).await?;
    let latest_season_id = season_windows.first().map(|window| window.season_id);

    let (source, season_id, from, to, fallback_applied) = match query.source {
        HeatQuerySource::AllTime => {
            let upper = at.map(|value| value.min(now)).unwrap_or(now);
            let earliest = load_owner_change_earliest(pool).await?;
            let from = earliest.map(|value| value.min(upper)).unwrap_or(upper);
            (
                HistoryHeatSource::AllTime,
                None,
                from,
                upper,
                false, // fallback is season-source only
            )
        }
        HeatQuerySource::Season => {
            if let Some(latest_id) = latest_season_id {
                let selected_id = query.season_id.unwrap_or(latest_id);
                let selected = season_windows
                    .iter()
                    .find(|window| window.season_id == selected_id)
                    .ok_or(StatusCode::BAD_REQUEST)?;
                let upper = if selected.season_id == latest_id {
                    now
                } else {
                    selected.end
                };
                let to = at.map(|value| value.min(upper)).unwrap_or(upper);
                (
                    HistoryHeatSource::Season,
                    Some(selected.season_id),
                    selected.start.min(to),
                    to,
                    false,
                )
            } else {
                let upper = at.map(|value| value.min(now)).unwrap_or(now);
                let fallback_start = now - chrono::Duration::days(HEAT_SEASON_FALLBACK_DAYS);
                (
                    HistoryHeatSource::Season,
                    None,
                    fallback_start.min(upper),
                    upper,
                    true,
                )
            }
        }
    };

    let mut entries = if to >= from {
        let rows: Vec<HeatCountRow> = sqlx::query_as(
            "SELECT territory, COUNT(*)::BIGINT AS take_count \
             FROM territory_events \
             WHERE prev_guild_uuid IS NOT NULL \
               AND recorded_at >= $1 \
               AND recorded_at <= $2 \
             GROUP BY territory",
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        rows.into_iter()
            .filter_map(|(territory, take_count)| {
                let take_count = u64::try_from(take_count).ok()?;
                Some(HistoryHeatEntry {
                    territory,
                    take_count,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    entries.sort_by(|a, b| {
        b.take_count
            .cmp(&a.take_count)
            .then_with(|| a.territory.cmp(&b.territory))
    });
    let max_take_count = entries
        .iter()
        .map(|entry| entry.take_count)
        .max()
        .unwrap_or(0);

    let response = HistoryHeat {
        source,
        season_id,
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        fallback_applied,
        max_take_count,
        entries,
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        if at.is_some() {
            HeaderValue::from_static("public, max-age=10")
        } else {
            HeaderValue::from_static("public, max-age=30")
        },
    );

    Ok((headers, Json(response)))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use chrono::{DateTime, Utc};
    use reqwest::StatusCode;
    use sequoia_shared::history::{
        HistoryEvents, HistoryHeat, HistoryHeatMeta, HistoryHeatSource, HistorySnapshot,
        HistorySrSamples,
    };
    use sqlx::postgres::PgPoolOptions;

    use super::HEAT_SEASON_FALLBACK_DAYS;
    use crate::config::territory_history_retention_days;
    use crate::state::AppState;

    const REAL_DB_TEST_LOCK: i64 = 73_019_001;

    fn lazy_test_pool() -> sqlx::PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://sequoia:sequoia@localhost/sequoia")
            .expect("lazy test pool should parse")
    }

    async fn spawn_test_server(state: AppState) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let app = crate::app::build_app(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn invalid_history_query_params_return_bad_request() {
        let state = AppState::new(Some(lazy_test_pool()));
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let at_status = client
            .get(format!("{base_url}/api/history/at?t=not-a-timestamp"))
            .send()
            .await
            .expect("history at request")
            .status();
        assert_eq!(at_status, StatusCode::BAD_REQUEST);

        let events_status = client
            .get(format!(
                "{base_url}/api/history/events?from=nope&to=also-nope"
            ))
            .send()
            .await
            .expect("history events request")
            .status();
        assert_eq!(events_status, StatusCode::BAD_REQUEST);

        let sr_samples_status = client
            .get(format!(
                "{base_url}/api/history/sr-samples?from=nope&to=also-nope"
            ))
            .send()
            .await
            .expect("history sr samples request")
            .status();
        assert_eq!(sr_samples_status, StatusCode::BAD_REQUEST);

        let heat_invalid_source_status = client
            .get(format!("{base_url}/api/history/heat?source=wat"))
            .send()
            .await
            .expect("history heat invalid source request")
            .status();
        assert_eq!(heat_invalid_source_status, StatusCode::BAD_REQUEST);

        let heat_invalid_at_status = client
            .get(format!(
                "{base_url}/api/history/heat?source=all_time&at=definitely-not-a-time"
            ))
            .send()
            .await
            .expect("history heat invalid at request")
            .status();
        assert_eq!(heat_invalid_at_status, StatusCode::BAD_REQUEST);

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn history_events_paginates_with_after_seq() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping real-Postgres history pagination test: DATABASE_URL is not set");
            return;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("connect real postgres");
        let mut lock_conn = pool.acquire().await.expect("acquire lock connection");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("acquire history test db lock");
        crate::db_migrations::run(&pool)
            .await
            .expect("run migrations");
        sqlx::query(
            "TRUNCATE TABLE territory_events, territory_snapshots, season_scalar_samples, season_guild_observations, guild_color_cache RESTART IDENTITY",
        )
            .execute(&pool)
            .await
            .expect("truncate history tables");

        let now = Utc::now();
        let recorded_1 = now - chrono::TimeDelta::minutes(1);
        let recorded_2 = now;
        let acquired_1 = recorded_1;
        let acquired_2 = recorded_2;
        sqlx::query(
            "INSERT INTO territory_events \
             (stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, guild_prefix, \
              prev_guild_uuid, prev_guild_name, prev_guild_prefix) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(1_i64)
        .bind(recorded_1)
        .bind(acquired_1)
        .bind("Alpha")
        .bind("g1")
        .bind("GuildOne")
        .bind("G1")
        .bind(None::<&str>)
        .bind(None::<&str>)
        .bind(None::<&str>)
        .execute(&pool)
        .await
        .expect("insert event 1");

        sqlx::query(
            "INSERT INTO territory_events \
             (stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, guild_prefix, \
              prev_guild_uuid, prev_guild_name, prev_guild_prefix) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(2_i64)
        .bind(recorded_2)
        .bind(acquired_2)
        .bind("Alpha")
        .bind("g2")
        .bind("GuildTwo")
        .bind("G2")
        .bind(Some("g1"))
        .bind(Some("GuildOne"))
        .bind(Some("G1"))
        .execute(&pool)
        .await
        .expect("insert event 2");

        sqlx::query(
            "INSERT INTO guild_color_cache (guild_name, color_r, color_g, color_b) \
             VALUES ($1, $2, $3, $4), ($5, $6, $7, $8)",
        )
        .bind("GuildOne")
        .bind(10_i16)
        .bind(20_i16)
        .bind(30_i16)
        .bind("GuildTwo")
        .bind(40_i16)
        .bind(50_i16)
        .bind(60_i16)
        .execute(&pool)
        .await
        .expect("insert guild color cache rows");

        let sr_sample_time = now - chrono::TimeDelta::seconds(30);
        sqlx::query(
            "INSERT INTO season_guild_observations \
             (observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, \
              season_rating, sr_gain_5m, sample_rank) \
             VALUES \
             ($1, $2, $3, $4, $5, $6, $7, $8, $9), \
             ($10, $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(sr_sample_time)
        .bind(29_i32)
        .bind("GuildTwo")
        .bind("g2")
        .bind("G2")
        .bind(6_i16)
        .bind(1200_i32)
        .bind(150_i32)
        .bind(1_i32)
        .bind(sr_sample_time)
        .bind(29_i32)
        .bind("GuildOne")
        .bind("g1")
        .bind("G1")
        .bind(4_i16)
        .bind(900_i32)
        .bind(100_i32)
        .bind(2_i32)
        .execute(&pool)
        .await
        .expect("insert season guild observations");

        let state = AppState::new(Some(pool.clone()));
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let from = (now - chrono::TimeDelta::hours(1)).to_rfc3339();
        let to = (now + chrono::TimeDelta::hours(1)).to_rfc3339();
        let all_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/events"))
                .expect("history events url");
            url.query_pairs_mut()
                .append_pair("from", &from)
                .append_pair("to", &to)
                .append_pair("limit", "100");
            url
        };
        let all_events = client
            .get(all_url)
            .send()
            .await
            .expect("all events request")
            .error_for_status()
            .expect("all events status")
            .json::<HistoryEvents>()
            .await
            .expect("parse all events");
        assert_eq!(all_events.events.len(), 2);
        assert_eq!(all_events.events[0].stream_seq, 1);
        assert_eq!(all_events.events[1].stream_seq, 2);
        assert_eq!(all_events.events[0].guild_color, Some((10, 20, 30)));
        assert_eq!(all_events.events[1].guild_color, Some((40, 50, 60)));
        assert_eq!(all_events.events[1].prev_guild_color, Some((10, 20, 30)));

        let sr_samples_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/sr-samples"))
                .expect("history sr samples url");
            url.query_pairs_mut()
                .append_pair("from", &from)
                .append_pair("to", &to);
            url
        };
        let sr_samples = client
            .get(sr_samples_url)
            .send()
            .await
            .expect("sr samples request")
            .error_for_status()
            .expect("sr samples status")
            .json::<HistorySrSamples>()
            .await
            .expect("parse sr samples");
        assert_eq!(sr_samples.samples.len(), 1);
        let sr_sample = &sr_samples.samples[0];
        assert_eq!(sr_sample.entries.len(), 2);
        assert_eq!(sr_sample.entries[0].guild_name, "GuildTwo");
        assert_eq!(sr_sample.entries[0].season_rank, 1);
        assert_eq!(sr_sample.entries[0].sr_gain_5m, Some(150_i64));

        let heat_meta = client
            .get(format!("{base_url}/api/history/heat/meta"))
            .send()
            .await
            .expect("heat meta request")
            .error_for_status()
            .expect("heat meta status")
            .json::<HistoryHeatMeta>()
            .await
            .expect("parse heat meta");
        assert_eq!(heat_meta.latest_season_id, Some(29));
        assert!(!heat_meta.seasons.is_empty());
        assert_eq!(heat_meta.seasons[0].season_id, 29);
        assert!(heat_meta.seasons[0].is_current);
        assert_eq!(heat_meta.retention_days, territory_history_retention_days());
        assert_eq!(heat_meta.season_fallback_days, HEAT_SEASON_FALLBACK_DAYS);

        let heat_season_url = {
            let mut url =
                reqwest::Url::parse(&format!("{base_url}/api/history/heat")).expect("heat url");
            url.query_pairs_mut()
                .append_pair("source", "season")
                .append_pair("season_id", "29");
            url
        };
        let heat_season = client
            .get(heat_season_url)
            .send()
            .await
            .expect("heat season request")
            .error_for_status()
            .expect("heat season status")
            .json::<HistoryHeat>()
            .await
            .expect("parse heat season");
        assert_eq!(heat_season.source, HistoryHeatSource::Season);
        assert_eq!(heat_season.season_id, Some(29));
        assert!(!heat_season.fallback_applied);
        assert_eq!(heat_season.max_take_count, 1);
        assert_eq!(heat_season.entries.len(), 1);
        assert_eq!(heat_season.entries[0].territory, "Alpha");
        assert_eq!(heat_season.entries[0].take_count, 1);

        let heat_all_time = client
            .get(format!("{base_url}/api/history/heat?source=all_time"))
            .send()
            .await
            .expect("heat all-time request")
            .error_for_status()
            .expect("heat all-time status")
            .json::<HistoryHeat>()
            .await
            .expect("parse heat all-time");
        assert_eq!(heat_all_time.source, HistoryHeatSource::AllTime);
        assert_eq!(heat_all_time.max_take_count, 1);
        assert_eq!(heat_all_time.entries.len(), 1);
        assert_eq!(heat_all_time.entries[0].territory, "Alpha");
        assert_eq!(heat_all_time.entries[0].take_count, 1);

        let heat_all_time_at_change_url = {
            let mut url =
                reqwest::Url::parse(&format!("{base_url}/api/history/heat")).expect("heat url");
            url.query_pairs_mut()
                .append_pair("source", "all_time")
                .append_pair("at", &recorded_2.to_rfc3339());
            url
        };
        let heat_all_time_at_change = client
            .get(heat_all_time_at_change_url)
            .send()
            .await
            .expect("heat all-time at request")
            .error_for_status()
            .expect("heat all-time at status")
            .json::<HistoryHeat>()
            .await
            .expect("parse heat all-time at");
        assert_eq!(heat_all_time_at_change.source, HistoryHeatSource::AllTime);
        assert_eq!(heat_all_time_at_change.max_take_count, 1);
        assert_eq!(heat_all_time_at_change.entries.len(), 1);
        assert_eq!(heat_all_time_at_change.entries[0].territory, "Alpha");
        assert_eq!(heat_all_time_at_change.entries[0].take_count, 1);

        let heat_cumulative_before_change_url = {
            let mut url =
                reqwest::Url::parse(&format!("{base_url}/api/history/heat")).expect("heat url");
            url.query_pairs_mut()
                .append_pair("source", "season")
                .append_pair("season_id", "29")
                .append_pair("at", &recorded_1.to_rfc3339());
            url
        };
        let heat_cumulative_before_change = client
            .get(heat_cumulative_before_change_url)
            .send()
            .await
            .expect("heat cumulative request")
            .error_for_status()
            .expect("heat cumulative status")
            .json::<HistoryHeat>()
            .await
            .expect("parse heat cumulative");
        assert_eq!(
            heat_cumulative_before_change.source,
            HistoryHeatSource::Season
        );
        assert_eq!(heat_cumulative_before_change.season_id, Some(29));
        assert_eq!(heat_cumulative_before_change.max_take_count, 0);
        assert!(heat_cumulative_before_change.entries.is_empty());

        let paged_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/events"))
                .expect("paged history events url");
            url.query_pairs_mut()
                .append_pair("from", &from)
                .append_pair("to", &to)
                .append_pair("after_seq", "1")
                .append_pair("limit", "100");
            url
        };
        let paged_events = client
            .get(paged_url)
            .send()
            .await
            .expect("paged events request")
            .error_for_status()
            .expect("paged events status")
            .json::<HistoryEvents>()
            .await
            .expect("parse paged events");
        assert_eq!(paged_events.events.len(), 1);
        assert_eq!(paged_events.events[0].stream_seq, 2);
        assert!(!paged_events.has_more);
        assert_eq!(paged_events.events[0].guild_color, Some((40, 50, 60)));
        assert_eq!(paged_events.events[0].prev_guild_color, Some((10, 20, 30)));

        let at_url = {
            let mut url =
                reqwest::Url::parse(&format!("{base_url}/api/history/at")).expect("history at url");
            url.query_pairs_mut().append_pair("t", &to);
            url
        };
        let snapshot = client
            .get(at_url)
            .send()
            .await
            .expect("history at request")
            .error_for_status()
            .expect("history at status")
            .json::<HistorySnapshot>()
            .await
            .expect("parse history snapshot");
        let alpha = snapshot
            .ownership
            .get("Alpha")
            .expect("alpha should exist in reconstructed snapshot");
        assert_eq!(alpha.guild_name, "GuildTwo");
        assert_eq!(alpha.guild_color, Some((40, 50, 60)));
        let season_leaderboard = snapshot
            .season_leaderboard
            .expect("season leaderboard should be present");
        assert_eq!(season_leaderboard.len(), 1);
        assert_eq!(season_leaderboard[0].guild_name, "GuildTwo");
        assert_eq!(season_leaderboard[0].season_rank, 1);
        assert_eq!(season_leaderboard[0].season_rating, 1200);

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("release history test db lock");

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn history_at_includes_latest_scalar_sample_at_or_before_timestamp() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping history scalar sample test: DATABASE_URL is not set");
            return;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("connect real postgres");
        let mut lock_conn = pool.acquire().await.expect("acquire lock connection");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("acquire history scalar test db lock");
        crate::db_migrations::run(&pool)
            .await
            .expect("run migrations");
        sqlx::query(
            "TRUNCATE TABLE territory_events, territory_snapshots, season_scalar_samples, season_guild_observations, guild_color_cache RESTART IDENTITY",
        )
        .execute(&pool)
        .await
        .expect("truncate tables");

        let before = Utc::now() - chrono::TimeDelta::minutes(30);
        let sample_time = Utc::now() - chrono::TimeDelta::minutes(5);
        let inferred_sample_time = Utc::now() - chrono::TimeDelta::minutes(1);
        let after = Utc::now();

        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(sample_time)
        .bind(29_i32)
        .bind(2.15_f64)
        .bind(2.40_f64)
        .bind(0.99_f64)
        .bind(1_i32)
        .execute(&pool)
        .await
        .expect("insert authoritative scalar sample");
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(inferred_sample_time)
        .bind(29_i32)
        .bind(9.99_f64)
        .bind(9.99_f64)
        .bind(0.42_f64)
        .bind(25_i32)
        .execute(&pool)
        .await
        .expect("insert inferred scalar sample");

        let state = AppState::new(Some(pool.clone()));
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let at_after_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/at"))
                .expect("history at url (after sample)");
            url.query_pairs_mut().append_pair("t", &after.to_rfc3339());
            url
        };
        let at_after = client
            .get(at_after_url)
            .send()
            .await
            .expect("history at request (after sample)")
            .error_for_status()
            .expect("history at status (after sample)")
            .json::<HistorySnapshot>()
            .await
            .expect("parse history snapshot (after sample)");

        let sample = at_after
            .season_scalar
            .expect("season scalar should be attached");
        assert_eq!(sample.season_id, 29);
        assert_eq!(sample.sample_count, 1);
        assert!((sample.scalar_weighted - 2.15).abs() < 1e-9);

        sqlx::query("TRUNCATE TABLE season_scalar_samples RESTART IDENTITY")
            .execute(&pool)
            .await
            .expect("truncate scalar samples");
        let fallback_time = Utc::now() - chrono::TimeDelta::seconds(30);
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(fallback_time)
        .bind(30_i32)
        .bind(1.91_f64)
        .bind(2.02_f64)
        .bind(0.44_f64)
        .bind(3_i32)
        .execute(&pool)
        .await
        .expect("insert fallback scalar sample");

        let fallback_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/at"))
                .expect("history at url (fallback sample)");
            url.query_pairs_mut()
                .append_pair("t", &Utc::now().to_rfc3339());
            url
        };
        let fallback_at = client
            .get(fallback_url)
            .send()
            .await
            .expect("history at request (fallback sample)")
            .error_for_status()
            .expect("history at status (fallback sample)")
            .json::<HistorySnapshot>()
            .await
            .expect("parse history snapshot (fallback sample)");

        let fallback_sample = fallback_at
            .season_scalar
            .expect("fallback scalar should be attached when authoritative sample is absent");
        assert_eq!(fallback_sample.season_id, 30);
        assert_eq!(fallback_sample.sample_count, 3);
        assert!((fallback_sample.confidence - 0.44).abs() < 1e-9);

        let at_before_url = {
            let mut url = reqwest::Url::parse(&format!("{base_url}/api/history/at"))
                .expect("history at url (before sample)");
            url.query_pairs_mut().append_pair("t", &before.to_rfc3339());
            url
        };
        let at_before = client
            .get(at_before_url)
            .send()
            .await
            .expect("history at request (before sample)")
            .error_for_status()
            .expect("history at status (before sample)")
            .json::<HistorySnapshot>()
            .await
            .expect("parse history snapshot (before sample)");

        assert!(at_before.season_scalar.is_none());

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("release history scalar test db lock");

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn history_heat_season_falls_back_to_last_sixty_days_without_season_metadata() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping history heat fallback test: DATABASE_URL is not set");
            return;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("connect real postgres");
        let mut lock_conn = pool.acquire().await.expect("acquire lock connection");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("acquire history heat fallback db lock");
        crate::db_migrations::run(&pool)
            .await
            .expect("run migrations");
        sqlx::query(
            "TRUNCATE TABLE territory_events, territory_snapshots, season_scalar_samples, season_guild_observations, guild_color_cache RESTART IDENTITY",
        )
        .execute(&pool)
        .await
        .expect("truncate tables");

        let now = Utc::now();
        let event_time = now - chrono::TimeDelta::days(5);
        sqlx::query(
            "INSERT INTO territory_events \
             (stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, guild_prefix, \
              prev_guild_uuid, prev_guild_name, prev_guild_prefix) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(1_i64)
        .bind(event_time)
        .bind(event_time)
        .bind("Beta")
        .bind("g3")
        .bind("GuildThree")
        .bind("G3")
        .bind(Some("g2"))
        .bind(Some("GuildTwo"))
        .bind(Some("G2"))
        .execute(&pool)
        .await
        .expect("insert owner-change event");

        // Should not be counted (no previous owner).
        sqlx::query(
            "INSERT INTO territory_events \
             (stream_seq, recorded_at, acquired_at, territory, guild_uuid, guild_name, guild_prefix, \
              prev_guild_uuid, prev_guild_name, prev_guild_prefix) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(2_i64)
        .bind(now - chrono::TimeDelta::days(2))
        .bind(now - chrono::TimeDelta::days(2))
        .bind("Gamma")
        .bind("g4")
        .bind("GuildFour")
        .bind("G4")
        .bind(None::<&str>)
        .bind(None::<&str>)
        .bind(None::<&str>)
        .execute(&pool)
        .await
        .expect("insert non-owner-change event");

        let state = AppState::new(Some(pool));
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let heat = client
            .get(format!("{base_url}/api/history/heat?source=season"))
            .send()
            .await
            .expect("heat fallback request")
            .error_for_status()
            .expect("heat fallback status")
            .json::<HistoryHeat>()
            .await
            .expect("parse heat fallback");
        assert_eq!(heat.source, HistoryHeatSource::Season);
        assert!(heat.fallback_applied);
        assert_eq!(heat.season_id, None);
        assert_eq!(heat.max_take_count, 1);
        assert_eq!(heat.entries.len(), 1);
        assert_eq!(heat.entries[0].territory, "Beta");
        assert_eq!(heat.entries[0].take_count, 1);

        let from = heat
            .from
            .parse::<DateTime<Utc>>()
            .expect("parse fallback from");
        let to = heat.to.parse::<DateTime<Utc>>().expect("parse fallback to");
        assert!(to >= from);
        let fallback_window_secs = (to - from).num_seconds();
        assert!(fallback_window_secs <= HEAT_SEASON_FALLBACK_DAYS * 24 * 3600);

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("release history heat fallback db lock");

        server_handle.abort();
        let _ = server_handle.await;
    }
}
