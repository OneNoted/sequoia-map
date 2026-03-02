use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use serde::Deserialize;
use sqlx::{Postgres, QueryBuilder};
use tracing::{info, warn};

use sequoia_shared::{SeasonScalarCurrent, SeasonScalarSample};

use crate::config::WYNNCRAFT_GUILD_URL;
use crate::state::AppState;

const OBSERVATION_INTERVAL_SECS: u64 = 300;
const LEADERBOARD_SAMPLE_GUILDS: usize = 50;
const AUTHORITATIVE_CONFIDENCE_MIN: f64 = 0.99;
const AUTHORITATIVE_SAMPLE_COUNT: i32 = 1;

type LatestScalarSampleRow = (DateTime<Utc>, i32, f64, f64, f64, i32);

#[derive(Debug, Clone)]
struct CandidateGuild {
    guild_name: String,
    guild_uuid: String,
    guild_prefix: String,
    territory_count: usize,
}

#[derive(Debug, Clone)]
struct GuildSeasonSnapshot {
    guild_name: String,
    guild_uuid: String,
    guild_prefix: String,
    observed_at: DateTime<Utc>,
    season_id: i32,
    rating: i64,
    territory_count: usize,
}

#[derive(Debug, Deserialize)]
struct GuildSeasonRank {
    rating: i64,
}

#[derive(Debug, Deserialize)]
struct GuildPayload {
    #[serde(default, rename = "seasonRanks")]
    season_ranks: HashMap<String, GuildSeasonRank>,
}

pub async fn run(state: AppState) {
    let Some(pool) = state.db.as_ref().cloned() else {
        warn!("season guild observation sampler disabled: no database configured");
        return;
    };

    info!(
        interval_secs = OBSERVATION_INTERVAL_SECS,
        leaderboard_sample_guilds = LEADERBOARD_SAMPLE_GUILDS,
        "season guild observation sampler started (scalar inference disabled; authoritative ingest only)"
    );

    warm_cache(&state).await;

    let mut interval = tokio::time::interval(Duration::from_secs(OBSERVATION_INTERVAL_SECS));

    loop {
        interval.tick().await;

        if let Err(e) = sample_once(&state, &pool).await {
            warn!(error = %e, "season guild observation sampler tick failed");
        }
    }
}

pub async fn warm_cache(state: &AppState) {
    let Some(pool) = state.db.as_ref() else {
        return;
    };
    if let Err(e) = refresh_latest_scalar_cache(state, pool).await {
        warn!(error = %e, "failed to warm authoritative season scalar cache from database");
    }
}

async fn sample_once(state: &AppState, pool: &sqlx::PgPool) -> Result<(), String> {
    let sampled_candidates = top_candidate_guilds(state, LEADERBOARD_SAMPLE_GUILDS).await;
    if sampled_candidates.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let futures = sampled_candidates
        .iter()
        .map(|candidate| fetch_guild_snapshot(&state.http_client, candidate, now));
    let snapshots: Vec<GuildSeasonSnapshot> =
        join_all(futures).await.into_iter().flatten().collect();
    if snapshots.is_empty() {
        return Ok(());
    }

    persist_guild_observations(pool, &snapshots).await
}

async fn persist_guild_observations(
    pool: &sqlx::PgPool,
    snapshots: &[GuildSeasonSnapshot],
) -> Result<(), String> {
    if snapshots.is_empty() {
        return Ok(());
    }

    #[derive(Debug)]
    struct ObservationInsertRow {
        observed_at: DateTime<Utc>,
        season_id: i32,
        guild_name: String,
        guild_uuid: String,
        guild_prefix: String,
        territory_count: i16,
        season_rating: i32,
        sr_gain_5m: Option<i32>,
        sample_rank: i32,
    }

    let guild_names: Vec<String> = snapshots.iter().map(|row| row.guild_name.clone()).collect();
    let latest_rows: Vec<(String, i32, i32)> = sqlx::query_as(
        "SELECT DISTINCT ON (guild_name) guild_name, season_id, season_rating \
         FROM season_guild_observations \
         WHERE guild_name = ANY($1) \
         ORDER BY guild_name, observed_at DESC",
    )
    .bind(&guild_names)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load latest season observations: {e}"))?;
    let latest_by_guild: HashMap<String, (i32, i32)> = latest_rows
        .into_iter()
        .map(|(guild_name, season_id, season_rating)| (guild_name, (season_id, season_rating)))
        .collect();
    let sample_ranks = rank_snapshots(snapshots);

    let mut rows = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let territory_count = i16::try_from(snapshot.territory_count).map_err(|_| {
            format!(
                "territory count {} is out of i16 range for guild {}",
                snapshot.territory_count, snapshot.guild_name
            )
        })?;
        let season_rating = i32::try_from(snapshot.rating).map_err(|_| {
            format!(
                "season rating {} is out of i32 range for guild {}",
                snapshot.rating, snapshot.guild_name
            )
        })?;
        let sr_gain_5m =
            latest_by_guild
                .get(&snapshot.guild_name)
                .and_then(|(season_id, previous_rating)| {
                    if *season_id != snapshot.season_id {
                        return None;
                    }
                    let delta = snapshot.rating - i64::from(*previous_rating);
                    i32::try_from(delta).ok()
                });
        let sample_rank = sample_ranks
            .get(&snapshot.guild_name)
            .copied()
            .ok_or_else(|| format!("missing sample rank for guild {}", snapshot.guild_name))?;

        rows.push(ObservationInsertRow {
            observed_at: snapshot.observed_at,
            season_id: snapshot.season_id,
            guild_name: snapshot.guild_name.clone(),
            guild_uuid: snapshot.guild_uuid.clone(),
            guild_prefix: snapshot.guild_prefix.clone(),
            territory_count,
            season_rating,
            sr_gain_5m,
            sample_rank,
        });
    }

    let mut query_builder = QueryBuilder::<Postgres>::new(
        "INSERT INTO season_guild_observations \
         (observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating, sr_gain_5m, sample_rank) ",
    );
    query_builder.push_values(rows, |mut builder, row| {
        builder
            .push_bind(row.observed_at)
            .push_bind(row.season_id)
            .push_bind(row.guild_name)
            .push_bind(row.guild_uuid)
            .push_bind(row.guild_prefix)
            .push_bind(row.territory_count)
            .push_bind(row.season_rating)
            .push_bind(row.sr_gain_5m)
            .push_bind(row.sample_rank);
    });
    query_builder.push(" ON CONFLICT (observed_at, guild_name) DO NOTHING");
    query_builder
        .build()
        .execute(pool)
        .await
        .map_err(|e| format!("insert season guild observations: {e}"))?;

    Ok(())
}

fn rank_snapshots(snapshots: &[GuildSeasonSnapshot]) -> HashMap<String, i32> {
    let mut ranked: Vec<&GuildSeasonSnapshot> = snapshots.iter().collect();
    ranked.sort_by(|a, b| {
        b.rating
            .cmp(&a.rating)
            .then_with(|| b.territory_count.cmp(&a.territory_count))
            .then_with(|| a.guild_name.cmp(&b.guild_name))
    });

    ranked
        .into_iter()
        .enumerate()
        .map(|(idx, row)| {
            let rank = i32::try_from(idx + 1).unwrap_or(i32::MAX);
            (row.guild_name.clone(), rank)
        })
        .collect()
}

async fn refresh_latest_scalar_cache(state: &AppState, pool: &sqlx::PgPool) -> Result<(), String> {
    let row: Option<LatestScalarSampleRow> = sqlx::query_as(
        "SELECT sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count \
         FROM season_scalar_samples \
         WHERE confidence >= $1 AND sample_count = $2 \
         ORDER BY sampled_at DESC \
         LIMIT 1",
    )
    .bind(AUTHORITATIVE_CONFIDENCE_MIN)
    .bind(AUTHORITATIVE_SAMPLE_COUNT)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("load latest authoritative season scalar sample: {e}"))?;

    let cached = row
        .map(
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
        .and_then(build_cached_scalar_sample);

    let mut latest = state.latest_scalar_sample.write().await;
    *latest = cached;
    Ok(())
}

fn build_cached_scalar_sample(
    sample: SeasonScalarSample,
) -> Option<(SeasonScalarSample, Arc<Bytes>)> {
    match serde_json::to_vec(&SeasonScalarCurrent {
        sample: Some(sample.clone()),
    }) {
        Ok(json) => Some((sample, Arc::new(Bytes::from(json)))),
        Err(e) => {
            warn!(error = %e, "failed to serialize season scalar cache payload");
            None
        }
    }
}

async fn top_candidate_guilds(state: &AppState, limit: usize) -> Vec<CandidateGuild> {
    let snapshot = state.live_snapshot.read().await;
    if snapshot.territories.is_empty() {
        return Vec::new();
    }

    let mut candidates: HashMap<String, CandidateGuild> = HashMap::new();
    for territory in snapshot.territories.values() {
        let guild_name = territory.guild.name.clone();
        let entry = candidates
            .entry(guild_name.clone())
            .or_insert_with(|| CandidateGuild {
                guild_name,
                guild_uuid: territory.guild.uuid.clone(),
                guild_prefix: territory.guild.prefix.clone(),
                territory_count: 0,
            });
        entry.territory_count += 1;
    }
    drop(snapshot);

    let mut guilds: Vec<CandidateGuild> = candidates.into_values().collect();
    guilds.sort_by(|a, b| {
        b.territory_count
            .cmp(&a.territory_count)
            .then_with(|| a.guild_name.cmp(&b.guild_name))
    });
    guilds.truncate(limit);
    guilds
}

async fn fetch_guild_snapshot(
    client: &reqwest::Client,
    candidate: &CandidateGuild,
    observed_at: DateTime<Utc>,
) -> Option<GuildSeasonSnapshot> {
    let mut url = match reqwest::Url::parse(WYNNCRAFT_GUILD_URL) {
        Ok(url) => url,
        Err(e) => {
            warn!(error = %e, "invalid guild base URL");
            return None;
        }
    };
    {
        let Ok(mut segments) = url.path_segments_mut() else {
            warn!("failed to edit guild URL path segments");
            return None;
        };
        segments.push(candidate.guild_name.as_str());
    }

    let response = match client.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!(guild = candidate.guild_name, error = %e, "guild fetch failed");
            return None;
        }
    };

    if !response.status().is_success() {
        warn!(
            guild = candidate.guild_name,
            status = response.status().as_u16(),
            "guild fetch returned non-success status"
        );
        return None;
    }

    let payload = match response.json::<GuildPayload>().await {
        Ok(payload) => payload,
        Err(e) => {
            warn!(guild = candidate.guild_name, error = %e, "guild response parse failed");
            return None;
        }
    };

    let (season_id, rating) = latest_season_rating(&payload.season_ranks)?;
    Some(GuildSeasonSnapshot {
        guild_name: candidate.guild_name.clone(),
        guild_uuid: candidate.guild_uuid.clone(),
        guild_prefix: candidate.guild_prefix.clone(),
        observed_at,
        season_id,
        rating,
        territory_count: candidate.territory_count,
    })
}

fn latest_season_rating(season_ranks: &HashMap<String, GuildSeasonRank>) -> Option<(i32, i64)> {
    season_ranks
        .iter()
        .filter_map(|(season, value)| season.parse::<i32>().ok().map(|id| (id, value.rating)))
        .max_by_key(|(season_id, _)| *season_id)
}

#[cfg(test)]
mod tests {
    use super::{GuildSeasonRank, GuildSeasonSnapshot, latest_season_rating, rank_snapshots};
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn latest_season_rating_uses_highest_season_key() {
        let ranks = HashMap::from([
            ("28".to_string(), GuildSeasonRank { rating: 150 }),
            ("29".to_string(), GuildSeasonRank { rating: 420 }),
            ("10".to_string(), GuildSeasonRank { rating: 99 }),
        ]);
        let latest = latest_season_rating(&ranks).expect("latest season should parse");
        assert_eq!(latest.0, 29);
        assert_eq!(latest.1, 420);
    }

    #[test]
    fn rank_snapshots_orders_by_rating_territories_then_name() {
        let now = Utc::now();
        let snapshots = vec![
            GuildSeasonSnapshot {
                guild_name: "Beta".to_string(),
                guild_uuid: "u2".to_string(),
                guild_prefix: "B".to_string(),
                observed_at: now,
                season_id: 29,
                rating: 2000,
                territory_count: 10,
            },
            GuildSeasonSnapshot {
                guild_name: "Alpha".to_string(),
                guild_uuid: "u1".to_string(),
                guild_prefix: "A".to_string(),
                observed_at: now,
                season_id: 29,
                rating: 2000,
                territory_count: 12,
            },
            GuildSeasonSnapshot {
                guild_name: "Gamma".to_string(),
                guild_uuid: "u3".to_string(),
                guild_prefix: "G".to_string(),
                observed_at: now,
                season_id: 29,
                rating: 1500,
                territory_count: 20,
            },
        ];

        let ranks = rank_snapshots(&snapshots);
        assert_eq!(ranks.get("Alpha"), Some(&1));
        assert_eq!(ranks.get("Beta"), Some(&2));
        assert_eq!(ranks.get("Gamma"), Some(&3));
    }
}
