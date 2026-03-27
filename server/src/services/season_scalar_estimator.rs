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

use crate::config::{WYNNCRAFT_GUILD_URL, season_rating_contender_count, season_rating_watchlist};
use crate::state::AppState;

const OBSERVATION_INTERVAL_SECS: u64 = 300;
const AUTHORITATIVE_CONFIDENCE_MIN: f64 = 0.99;
const AUTHORITATIVE_SAMPLE_COUNT_MIN: i32 = 1;

type LatestScalarSampleRow = (DateTime<Utc>, i32, f64, f64, f64, i32);
type LatestObservationCandidateRow = (String, String, String, i16);

#[derive(Debug, Clone, PartialEq, Eq)]
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
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
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
        contender_count = season_rating_contender_count(),
        watchlist_size = season_rating_watchlist().len(),
        "season guild observation sampler started"
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
    let sampled_candidates = top_candidate_guilds(
        state,
        pool,
        season_rating_contender_count(),
        &season_rating_watchlist(),
    )
    .await?;
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
         ORDER BY (confidence >= $1 AND sample_count >= $2) DESC, sampled_at DESC \
         LIMIT 1",
    )
    .bind(AUTHORITATIVE_CONFIDENCE_MIN)
    .bind(AUTHORITATIVE_SAMPLE_COUNT_MIN)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("load latest preferred season scalar sample: {e}"))?;

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

async fn top_candidate_guilds(
    state: &AppState,
    pool: &sqlx::PgPool,
    contender_count: usize,
    watchlist: &[String],
) -> Result<Vec<CandidateGuild>, String> {
    let snapshot = state.live_snapshot.read().await;
    if snapshot.territories.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates: HashMap<String, CandidateGuild> = HashMap::new();
    let mut live_candidates: HashMap<String, CandidateGuild> = HashMap::new();
    for territory in snapshot.territories.values() {
        accumulate_live_candidate(
            &mut live_candidates,
            &territory.guild.name,
            &territory.guild.uuid,
            &territory.guild.prefix,
        );
    }
    drop(snapshot);

    for candidate in live_candidates.into_values() {
        merge_candidate(&mut candidates, candidate);
    }

    for contender in latest_top_contender_guilds(pool, contender_count).await? {
        merge_candidate(&mut candidates, contender);
    }

    let watchlist_details = latest_observed_guilds_by_name(pool, watchlist).await?;
    for guild_name in watchlist {
        if let Some(candidate) = watchlist_details.get(guild_name) {
            merge_candidate(&mut candidates, candidate.clone());
        } else {
            merge_candidate(
                &mut candidates,
                CandidateGuild {
                    guild_name: guild_name.clone(),
                    guild_uuid: String::new(),
                    guild_prefix: String::new(),
                    territory_count: 0,
                },
            );
        }
    }

    let mut guilds: Vec<CandidateGuild> = candidates.into_values().collect();
    guilds.sort_by(|a, b| {
        b.territory_count
            .cmp(&a.territory_count)
            .then_with(|| a.guild_name.cmp(&b.guild_name))
    });
    Ok(guilds)
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
        guild_name: payload
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| candidate.guild_name.clone()),
        guild_uuid: payload
            .uuid
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| candidate.guild_uuid.clone()),
        guild_prefix: payload
            .prefix
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| candidate.guild_prefix.clone()),
        observed_at,
        season_id,
        rating,
        territory_count: candidate.territory_count,
    })
}

async fn latest_top_contender_guilds(
    pool: &sqlx::PgPool,
    contender_count: usize,
) -> Result<Vec<CandidateGuild>, String> {
    if contender_count == 0 {
        return Ok(Vec::new());
    }

    let rows: Vec<LatestObservationCandidateRow> = sqlx::query_as(
        "SELECT guild_name, COALESCE(guild_uuid, ''), COALESCE(guild_prefix, ''), territory_count \
         FROM ( \
             SELECT DISTINCT ON (guild_name) guild_name, guild_uuid, guild_prefix, territory_count, season_rating \
             FROM season_guild_observations \
             WHERE season_id = (SELECT MAX(season_id) FROM season_guild_observations) \
             ORDER BY guild_name, observed_at DESC \
         ) latest \
         ORDER BY season_rating DESC, territory_count DESC, guild_name ASC \
         LIMIT $1",
    )
    .bind(i64::try_from(contender_count).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load latest top contender guilds: {e}"))?;

    Ok(rows
        .into_iter()
        .map(candidate_from_observation_row)
        .collect())
}

async fn latest_observed_guilds_by_name(
    pool: &sqlx::PgPool,
    guild_names: &[String],
) -> Result<HashMap<String, CandidateGuild>, String> {
    if guild_names.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<LatestObservationCandidateRow> = sqlx::query_as(
        "SELECT guild_name, COALESCE(guild_uuid, ''), COALESCE(guild_prefix, ''), territory_count \
         FROM ( \
             SELECT DISTINCT ON (guild_name) guild_name, guild_uuid, guild_prefix, territory_count \
             FROM season_guild_observations \
             WHERE season_id = (SELECT MAX(season_id) FROM season_guild_observations) \
               AND guild_name = ANY($1) \
             ORDER BY guild_name, observed_at DESC \
         ) latest",
    )
    .bind(guild_names)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load latest observed guild details: {e}"))?;

    Ok(rows
        .into_iter()
        .map(candidate_from_observation_row)
        .map(|candidate| (candidate.guild_name.clone(), candidate))
        .collect())
}

fn candidate_from_observation_row(row: LatestObservationCandidateRow) -> CandidateGuild {
    let (guild_name, guild_uuid, guild_prefix, territory_count) = row;
    CandidateGuild {
        guild_name,
        guild_uuid,
        guild_prefix,
        territory_count: usize::try_from(territory_count.max(0)).unwrap_or(0),
    }
}

fn accumulate_live_candidate(
    target: &mut HashMap<String, CandidateGuild>,
    guild_name: &str,
    guild_uuid: &str,
    guild_prefix: &str,
) {
    use std::collections::hash_map::Entry;

    match target.entry(guild_name.to_string()) {
        Entry::Vacant(entry) => {
            entry.insert(CandidateGuild {
                guild_name: guild_name.to_string(),
                guild_uuid: guild_uuid.to_string(),
                guild_prefix: guild_prefix.to_string(),
                territory_count: 1,
            });
        }
        Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            existing.territory_count = existing.territory_count.saturating_add(1);
            if existing.guild_uuid.trim().is_empty() && !guild_uuid.trim().is_empty() {
                existing.guild_uuid = guild_uuid.to_string();
            }
            if existing.guild_prefix.trim().is_empty() && !guild_prefix.trim().is_empty() {
                existing.guild_prefix = guild_prefix.to_string();
            }
        }
    }
}

fn merge_candidate(target: &mut HashMap<String, CandidateGuild>, candidate: CandidateGuild) {
    use std::collections::hash_map::Entry;

    match target.entry(candidate.guild_name.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(candidate);
        }
        Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            if existing.guild_uuid.trim().is_empty() && !candidate.guild_uuid.trim().is_empty() {
                existing.guild_uuid = candidate.guild_uuid;
            }
            if existing.guild_prefix.trim().is_empty() && !candidate.guild_prefix.trim().is_empty()
            {
                existing.guild_prefix = candidate.guild_prefix;
            }
            existing.territory_count = existing.territory_count.max(candidate.territory_count);
        }
    }
}

fn latest_season_rating(season_ranks: &HashMap<String, GuildSeasonRank>) -> Option<(i32, i64)> {
    season_ranks
        .iter()
        .filter_map(|(season, value)| season.parse::<i32>().ok().map(|id| (id, value.rating)))
        .max_by_key(|(season_id, _)| *season_id)
}

#[cfg(test)]
mod tests {
    use super::{
        CandidateGuild, GuildSeasonRank, GuildSeasonSnapshot, accumulate_live_candidate,
        latest_season_rating, merge_candidate, rank_snapshots,
    };
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

    #[test]
    fn merge_candidate_preserves_identity_and_highest_territory_count() {
        let mut candidates = HashMap::new();
        merge_candidate(
            &mut candidates,
            CandidateGuild {
                guild_name: "Sequoia".to_string(),
                guild_uuid: String::new(),
                guild_prefix: String::new(),
                territory_count: 3,
            },
        );
        merge_candidate(
            &mut candidates,
            CandidateGuild {
                guild_name: "Sequoia".to_string(),
                guild_uuid: "uuid-1".to_string(),
                guild_prefix: "SEQ".to_string(),
                territory_count: 1,
            },
        );

        assert_eq!(
            candidates.get("Sequoia"),
            Some(&CandidateGuild {
                guild_name: "Sequoia".to_string(),
                guild_uuid: "uuid-1".to_string(),
                guild_prefix: "SEQ".to_string(),
                territory_count: 3,
            })
        );
    }

    #[test]
    fn accumulate_live_candidate_counts_each_live_territory() {
        let mut candidates = HashMap::new();
        accumulate_live_candidate(&mut candidates, "Sequoia", "", "");
        accumulate_live_candidate(&mut candidates, "Sequoia", "uuid-1", "SEQ");
        accumulate_live_candidate(&mut candidates, "Sequoia", "", "");

        assert_eq!(
            candidates.get("Sequoia"),
            Some(&CandidateGuild {
                guild_name: "Sequoia".to_string(),
                guild_uuid: "uuid-1".to_string(),
                guild_prefix: "SEQ".to_string(),
                territory_count: 3,
            })
        );
    }
}
