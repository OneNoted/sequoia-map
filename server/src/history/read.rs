//! Bounded history reads. Each query touches at most one checkpoint interval of facts, one
//! page, or the per-day takeover rollup plus two partial days.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveTime, Utc};
use sequoia_shared::history::OwnershipRecord;

use super::checkpoint;
use super::{from_micros, unpack_color};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Checkpoint(#[from] checkpoint::CheckpointError),
}

/// Ownership at `at`: the latest checkpoint at or before it, then every fact recorded after
/// the checkpoint and at or before `at`, in stream order. The legacy reconstruction used the
/// same time filter and order, so imported rows whose time and sequence disagree keep their
/// historical answer.
pub async fn ownership_at(
    pool: &sqlx::PgPool,
    at: DateTime<Utc>,
) -> Result<HashMap<String, OwnershipRecord>, ReadError> {
    let mut tx = snapshot(pool).await?;
    let base: Option<(DateTime<Utc>, Vec<u8>)> = sqlx::query_as(
        "SELECT taken_at, ownership FROM history_checkpoint WHERE taken_at <= $1 \
         ORDER BY taken_at DESC, id DESC LIMIT 1",
    )
    .bind(at)
    .fetch_optional(&mut *tx)
    .await?;
    let (since, holdings) = match base {
        Some((taken_at, bytes)) => (taken_at, checkpoint::decode(&bytes)?),
        None => (super::beginning(), Vec::new()),
    };
    let mut owners: HashMap<i16, (i32, DateTime<Utc>)> = holdings
        .into_iter()
        .map(|holding| {
            (
                holding.territory_id,
                (holding.guild_id, from_micros(holding.acquired_micros)),
            )
        })
        .collect();
    let replay: Vec<(i16, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT territory_id, guild_id, acquired_at FROM history_fact \
         WHERE recorded_at > $1 AND recorded_at <= $2 ORDER BY stream_seq, ordinal",
    )
    .bind(since)
    .bind(at)
    .fetch_all(&mut *tx)
    .await?;
    for (territory_id, guild_id, acquired_at) in replay {
        owners.insert(territory_id, (guild_id, acquired_at));
    }

    let territory_ids = sorted_unique(owners.keys().copied());
    let guild_ids = sorted_unique(owners.values().map(|(guild_id, _)| *guild_id));
    let (names, guilds) = resolve(&mut tx, &territory_ids, &guild_ids).await?;
    tx.commit().await?;

    Ok(owners
        .into_iter()
        .filter_map(|(territory_id, (guild_id, acquired_at))| {
            let name = names.get(&territory_id)?;
            let (uuid, guild_name, prefix, color) = guilds.get(&guild_id)?;
            Some((
                name.clone(),
                OwnershipRecord {
                    guild_uuid: uuid.clone(),
                    guild_name: guild_name.clone(),
                    guild_prefix: prefix.clone(),
                    guild_color: unpack_color(*color),
                    acquired_at: acquired_at.to_rfc3339(),
                },
            ))
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub struct FactRow {
    pub stream_seq: i64,
    pub ordinal: i16,
    pub recorded_at: DateTime<Utc>,
    pub acquired_at: DateTime<Utc>,
    pub territory: String,
    pub guild_uuid: String,
    pub guild_name: String,
    pub guild_prefix: String,
    pub guild_color: Option<(u8, u8, u8)>,
    pub prev_guild_name: Option<String>,
    pub prev_guild_prefix: Option<String>,
    pub prev_guild_color: Option<(u8, u8, u8)>,
}

type FactTuple = (
    i64,
    i16,
    DateTime<Utc>,
    DateTime<Utc>,
    i16,
    i32,
    Option<i32>,
);
type Guild = (String, String, String, Option<i32>);

pub struct FactPage {
    pub facts: Vec<FactRow>,
    pub has_more: bool,
}

/// One consistent read-only snapshot for a multi-statement read.
async fn snapshot(
    pool: &sqlx::PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Names and guild identities for the given dictionary ids, by primary key: a page or a
/// seek touches a few hundred of them, far fewer than the dictionaries hold.
async fn resolve(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    territory_ids: &[i16],
    guild_ids: &[i32],
) -> Result<(HashMap<i16, String>, HashMap<i32, Guild>), sqlx::Error> {
    let names = sqlx::query_as("SELECT id, name FROM history_territory WHERE id = ANY($1)")
        .bind(territory_ids)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
    let guilds = sqlx::query_as::<_, (i32, String, String, String, Option<i32>)>(
        "SELECT id, uuid, name, prefix, color FROM history_guild WHERE id = ANY($1)",
    )
    .bind(guild_ids)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|(id, uuid, name, prefix, color)| (id, (uuid, name, prefix, color)))
    .collect();
    Ok((names, guilds))
}

fn sorted_unique<T: Ord + Copy>(values: impl Iterator<Item = T>) -> Vec<T> {
    let mut values: Vec<T> = values.collect();
    values.sort_unstable();
    values.dedup();
    values
}

/// Facts recorded in `(from, to]` after the cursor, in stream order. A page never ends inside
/// a stream position, so a client that resumes after a sequence number alone still sees the
/// rest of a batch.
pub async fn fact_page(
    pool: &sqlx::PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    after: (i64, i16),
    limit: i64,
) -> Result<FactPage, sqlx::Error> {
    const SELECT: &str = "SELECT stream_seq, ordinal, recorded_at, acquired_at, territory_id, \
            guild_id, prev_guild_id FROM history_fact";
    let mut tx = snapshot(pool).await?;
    let mut rows: Vec<FactTuple> = sqlx::query_as(&format!(
        "{SELECT} WHERE (stream_seq, ordinal) > ($1, $2) \
           AND recorded_at > $3 AND recorded_at <= $4 \
         ORDER BY stream_seq, ordinal LIMIT $5"
    ))
    .bind(after.0)
    .bind(after.1)
    .bind(from)
    .bind(to)
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    let mut has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
        if let Some(&(seq, ordinal, ..)) = rows.last() {
            let rest: Vec<FactTuple> = sqlx::query_as(&format!(
                "{SELECT} WHERE stream_seq = $1 AND ordinal > $2 \
                   AND recorded_at > $3 AND recorded_at <= $4 ORDER BY ordinal"
            ))
            .bind(seq)
            .bind(ordinal)
            .bind(from)
            .bind(to)
            .fetch_all(&mut *tx)
            .await?;
            rows.extend(rest);
            has_more = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM history_fact WHERE stream_seq > $1 \
                   AND recorded_at > $2 AND recorded_at <= $3)",
            )
            .bind(seq)
            .bind(from)
            .bind(to)
            .fetch_one(&mut *tx)
            .await?;
        }
    }
    let territory_ids = sorted_unique(rows.iter().map(|row| row.4));
    let guild_ids = sorted_unique(rows.iter().flat_map(|row| [Some(row.5), row.6]).flatten());
    let (names, guilds) = resolve(&mut tx, &territory_ids, &guild_ids).await?;
    tx.commit().await?;

    let facts = rows
        .into_iter()
        .map(
            |(stream_seq, ordinal, recorded_at, acquired_at, territory_id, guild_id, prev_id)| {
                let missing = || sqlx::Error::RowNotFound;
                let territory = names.get(&territory_id).ok_or_else(missing)?.clone();
                let (guild_uuid, guild_name, guild_prefix, guild_color) =
                    guilds.get(&guild_id).ok_or_else(missing)?.clone();
                let previous = prev_id.and_then(|id| guilds.get(&id));
                Ok(FactRow {
                    stream_seq,
                    ordinal,
                    recorded_at,
                    acquired_at,
                    territory,
                    guild_uuid,
                    guild_name,
                    guild_prefix,
                    guild_color: unpack_color(guild_color),
                    prev_guild_name: previous.map(|guild| guild.1.clone()),
                    prev_guild_prefix: previous.map(|guild| guild.2.clone()),
                    prev_guild_color: previous.and_then(|guild| unpack_color(guild.3)),
                })
            },
        )
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(FactPage { facts, has_more })
}

pub struct Bounds {
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub earliest_take: Option<DateTime<Utc>>,
    pub fact_count: i64,
    pub latest_seq: Option<i64>,
}

pub async fn bounds(pool: &sqlx::PgPool) -> Result<Bounds, sqlx::Error> {
    let (earliest, latest, earliest_take, fact_count, latest_seq) = sqlx::query_as(
        "SELECT earliest_at, latest_at, earliest_take_at, fact_count, latest_fact_seq \
         FROM history_meta",
    )
    .fetch_one(pool)
    .await?;
    Ok(Bounds {
        earliest,
        latest,
        earliest_take,
        fact_count,
        latest_seq,
    })
}

/// Owner changes per territory recorded in `[from, to]`: whole UTC days from the rollup and
/// the partial days at either end from the facts themselves.
pub async fn takes_between(
    pool: &sqlx::PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let day_start = |at: DateTime<Utc>| at.date_naive().and_time(NaiveTime::MIN).and_utc();
    let first_whole = if day_start(from) == from {
        from
    } else {
        day_start(from) + Duration::days(1)
    };
    let end_whole = day_start(to + Duration::microseconds(1));
    let (whole_from, whole_to) = if first_whole < end_whole {
        (first_whole, end_whole)
    } else {
        (
            to + Duration::microseconds(1),
            to + Duration::microseconds(1),
        )
    };
    sqlx::query_as(
        "SELECT t.name, sum(c.takes)::int8 FROM ( \
             SELECT territory_id, takes::int8 AS takes FROM history_heat_day \
             WHERE day >= ($3 AT TIME ZONE 'UTC')::date AND day < ($4 AT TIME ZONE 'UTC')::date \
             UNION ALL \
             SELECT territory_id, count(*) FROM history_fact \
             WHERE take AND recorded_at >= $1 AND recorded_at <= $2 \
               AND (recorded_at < $3 OR recorded_at >= $4) \
             GROUP BY territory_id \
         ) c JOIN history_territory t ON t.id = c.territory_id GROUP BY t.name",
    )
    .bind(from)
    .bind(to)
    .bind(whole_from)
    .bind(whole_to)
    .fetch_all(pool)
    .await
}

/// Each season's first and last observation, one index probe per season.
pub async fn season_windows(
    pool: &sqlx::PgPool,
) -> Result<Vec<(i32, DateTime<Utc>, DateTime<Utc>)>, sqlx::Error> {
    sqlx::query_as(
        "WITH RECURSIVE seasons(id) AS ( \
             SELECT max(season_id) FROM season_guild_observations \
             UNION ALL \
             SELECT (SELECT max(season_id) FROM season_guild_observations WHERE season_id < s.id) \
             FROM seasons s WHERE s.id IS NOT NULL \
         ) \
         SELECT id, \
             (SELECT min(observed_at) FROM season_guild_observations WHERE season_id = id), \
             (SELECT max(observed_at) FROM season_guild_observations WHERE season_id = id) \
         FROM seasons WHERE id IS NOT NULL ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await
}

/// Remove history older than `cutoff` while keeping the latest checkpoint at or before it,
/// so the first retained fact still has its base. Returns removed facts and checkpoints.
pub async fn expire_before(
    pool: &sqlx::PgPool,
    cutoff: DateTime<Utc>,
) -> Result<(i64, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(anchor): Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(taken_at) FROM history_checkpoint WHERE taken_at <= $1")
            .bind(cutoff)
            .fetch_one(&mut *tx)
            .await?
    else {
        return Ok((0, 0));
    };
    let facts = sqlx::query("DELETE FROM history_fact WHERE recorded_at <= $1")
        .bind(anchor)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let checkpoints = sqlx::query("DELETE FROM history_checkpoint WHERE taken_at < $1")
        .bind(anchor)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if facts > 0 {
        sqlx::query("DELETE FROM history_heat_day WHERE day <= ($1 AT TIME ZONE 'UTC')::date")
            .bind(anchor)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO history_heat_day (day, territory_id, takes) \
             SELECT ($1 AT TIME ZONE 'UTC')::date, territory_id, count(*) FROM history_fact \
             WHERE take AND recorded_at > $1 \
               AND recorded_at < (($1 AT TIME ZONE 'UTC')::date + 1)::timestamp AT TIME ZONE 'UTC' \
             GROUP BY territory_id",
        )
        .bind(anchor)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE history_meta SET fact_count = fact_count - $1, \
                 earliest_at = (SELECT min(recorded_at) FROM history_fact), \
                 earliest_take_at = (SELECT min(recorded_at) FROM history_fact WHERE take)",
        )
        .bind(i64::try_from(facts).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok((
        i64::try_from(facts).unwrap_or(i64::MAX),
        i64::try_from(checkpoints).unwrap_or(i64::MAX),
    ))
}
