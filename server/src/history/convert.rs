//! Conversion between the legacy history tables and the compact store, both ways.
//!
//! Forward conversion is resumable and idempotent: each chunk commits with its source
//! watermark in `history_meta`. It can run while the previous server still writes legacy
//! rows; the new server converts the remaining delta at startup before it records anything.
//! `rollback` copies what the new store recorded after the legacy tables stopped back into
//! them, so the previous image can resume with the interval since cutover.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::info;

use super::archive::{self, Columns, Report};
use super::checkpoint::{self, Holding};
use super::writer::{GuildKey, insert_guilds, insert_territories};
use super::{CHECKPOINT_FACT_BUDGET, micros, pack_color, unpack_color};

const EVENT_CHUNK: i64 = 250_000;
const SNAPSHOT_CHUNK: i64 = 200;
const ARCHIVE_CHUNK: i64 = 20_000;

#[derive(Debug, Default)]
pub struct ConvertReport {
    pub events: u64,
    pub snapshots: u64,
    pub derived_checkpoints: u64,
    pub archive_rows: u64,
    pub archive_raw_rows: u64,
}

/// Convert every legacy event and snapshot above the stored watermarks, then add derived
/// checkpoints wherever a checkpoint interval holds more than the replay budget.
pub async fn convert_history(pool: &sqlx::PgPool) -> Result<ConvertReport, sqlx::Error> {
    let mut report = ConvertReport::default();
    let events = super::table_exists(pool, "territory_events").await?;
    let snapshots = super::table_exists(pool, "territory_snapshots").await?;
    let archive = super::table_exists(pool, "canonical_territory_updates").await?;
    if events {
        loop {
            let (done, through): (i64, Option<i64>) = sqlx::query_as(
                "SELECT legacy_events_through, (SELECT max(stream_seq) FROM territory_events) \
                 FROM history_meta",
            )
            .fetch_one(pool)
            .await?;
            let Some(through) = through.filter(|through| *through > done) else {
                break;
            };
            let end = through.min(done.saturating_add(EVENT_CHUNK));
            report.events += convert_event_chunk(pool, done, end).await?;
            info!(through = end, "converted legacy history events");
        }
    }
    if snapshots {
        loop {
            let converted = convert_snapshot_chunk(pool).await?;
            if converted == 0 {
                break;
            }
            report.snapshots += converted;
        }
    }
    report.derived_checkpoints = derive_checkpoints(pool).await?;
    // New archive reports take ids above every legacy one, converted or not yet.
    let legacy_archive_id: i64 = if archive {
        sqlx::query_scalar("SELECT coalesce(max(id), 0) FROM canonical_territory_updates")
            .fetch_one(pool)
            .await?
    } else {
        0
    };
    sqlx::query(
        "SELECT setval('canonical_report_id_seq', \
             GREATEST((SELECT coalesce(max(id), 0) FROM canonical_report), $1, 1))",
    )
    .bind(legacy_archive_id)
    .execute(pool)
    .await?;
    Ok(report)
}

async fn convert_event_chunk(
    pool: &sqlx::PgPool,
    after: i64,
    end: i64,
) -> Result<u64, sqlx::Error> {
    const COLOR: &str = "(e.guild_color_r::int << 16) | (e.guild_color_g::int << 8) \
                         | e.guild_color_b::int";
    const PREV_COLOR: &str = "(e.prev_guild_color_r::int << 16) \
                              | (e.prev_guild_color_g::int << 8) | e.prev_guild_color_b::int";
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO history_territory (name) SELECT DISTINCT e.territory FROM territory_events e \
         WHERE e.stream_seq > $1 AND e.stream_seq <= $2 AND NOT EXISTS \
             (SELECT 1 FROM history_territory t WHERE t.name = e.territory) \
         ORDER BY 1 ON CONFLICT DO NOTHING",
    )
    .bind(after)
    .bind(end)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO history_guild (uuid, name, prefix, color) \
         SELECT DISTINCT * FROM ( \
             SELECT e.guild_uuid, e.guild_name, e.guild_prefix, {COLOR} FROM territory_events e \
             WHERE e.stream_seq > $1 AND e.stream_seq <= $2 \
             UNION ALL \
             SELECT e.prev_guild_uuid, e.prev_guild_name, e.prev_guild_prefix, {PREV_COLOR} \
             FROM territory_events e \
             WHERE e.stream_seq > $1 AND e.stream_seq <= $2 AND e.prev_guild_uuid IS NOT NULL \
         ) s(uuid, name, prefix, color) WHERE NOT EXISTS (SELECT 1 FROM history_guild g \
             WHERE g.uuid = s.uuid AND g.name = s.name AND g.prefix = s.prefix \
               AND g.color IS NOT DISTINCT FROM s.color) \
         ON CONFLICT DO NOTHING"
    ))
    .bind(after)
    .bind(end)
    .execute(&mut *tx)
    .await?;
    let inserted: i64 = sqlx::query_scalar(&format!(
        "WITH inserted AS ( \
             INSERT INTO history_fact (stream_seq, recorded_at, acquired_at, guild_id, \
                 prev_guild_id, ordinal, territory_id, source, take) \
             SELECT e.stream_seq, e.recorded_at, e.acquired_at, g.id, p.id, 0, t.id, 0, \
                 e.prev_guild_uuid IS NOT NULL AND e.prev_guild_uuid <> e.guild_uuid \
             FROM territory_events e \
             JOIN history_territory t ON t.name = e.territory \
             JOIN history_guild g ON g.uuid = e.guild_uuid AND g.name = e.guild_name \
                 AND g.prefix = e.guild_prefix AND coalesce(g.color, -1) = coalesce({COLOR}, -1) \
             LEFT JOIN history_guild p ON p.uuid = e.prev_guild_uuid \
                 AND p.name = e.prev_guild_name AND p.prefix = e.prev_guild_prefix \
                 AND coalesce(p.color, -1) = coalesce({PREV_COLOR}, -1) \
             WHERE e.stream_seq > $1 AND e.stream_seq <= $2 \
             ON CONFLICT DO NOTHING \
             RETURNING stream_seq, recorded_at, territory_id, take \
         ), heat AS ( \
             INSERT INTO history_heat_day (day, territory_id, takes) \
             SELECT (recorded_at AT TIME ZONE 'UTC')::date, territory_id, count(*) \
             FROM inserted WHERE take GROUP BY 1, 2 \
             ON CONFLICT (day, territory_id) \
             DO UPDATE SET takes = history_heat_day.takes + EXCLUDED.takes \
         ), meta AS ( \
             UPDATE history_meta SET \
                 legacy_events_through = $2, \
                 fact_count = fact_count + (SELECT count(*) FROM inserted), \
                 earliest_at = LEAST(earliest_at, (SELECT min(recorded_at) FROM inserted)), \
                 latest_at = GREATEST(latest_at, (SELECT max(recorded_at) FROM inserted)), \
                 earliest_take_at = LEAST(earliest_take_at, \
                     (SELECT min(recorded_at) FROM inserted WHERE take)), \
                 latest_fact_seq = GREATEST(latest_fact_seq, \
                     (SELECT max(stream_seq) FROM inserted)) \
         ) SELECT count(*) FROM inserted"
    ))
    .bind(after)
    .bind(end)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(u64::try_from(inserted).unwrap_or(0))
}

#[derive(serde::Deserialize)]
struct LegacyOwnership {
    guild_uuid: String,
    guild_name: String,
    guild_prefix: String,
    #[serde(default)]
    guild_color: Option<(u8, u8, u8)>,
    acquired_at: String,
}

fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

async fn convert_snapshot_chunk(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let done: i64 = sqlx::query_scalar("SELECT legacy_snapshots_through FROM history_meta")
        .fetch_one(&mut *tx)
        .await?;
    let rows: Vec<(i64, DateTime<Utc>, Value)> = sqlx::query_as(
        "SELECT id, created_at, ownership FROM territory_snapshots WHERE id > $1 \
         ORDER BY id LIMIT $2",
    )
    .bind(done)
    .bind(SNAPSHOT_CHUNK)
    .fetch_all(&mut *tx)
    .await?;
    let Some(&(last_id, ..)) = rows.last() else {
        return Ok(0);
    };

    let mut parsed = Vec::with_capacity(rows.len());
    for (id, created_at, ownership) in rows {
        let ownership: HashMap<String, LegacyOwnership> = serde_json::from_value(ownership)
            .map_err(|error| {
                sqlx::Error::Protocol(format!("legacy snapshot {id} is not ownership: {error}"))
            })?;
        parsed.push((id, created_at, ownership));
    }
    let mut names: Vec<String> = parsed
        .iter()
        .flat_map(|(_, _, ownership)| ownership.keys().cloned())
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut keys: Vec<GuildKey> = parsed
        .iter()
        .flat_map(|(_, _, ownership)| ownership.values().map(legacy_guild_key))
        .collect();
    keys.sort_unstable_by(|a, b| {
        (&a.uuid, &a.name, &a.prefix, a.color).cmp(&(&b.uuid, &b.name, &b.prefix, b.color))
    });
    keys.dedup();
    let territories = insert_territories(&mut tx, &names).await?;
    let guilds = insert_guilds(&mut tx, &keys).await?;

    for (id, created_at, ownership) in &parsed {
        let mut holdings = ownership
            .iter()
            .map(|(name, record)| {
                let acquired = parse_time(&record.acquired_at).ok_or_else(|| {
                    sqlx::Error::Protocol(format!(
                        "legacy snapshot {id} has an unreadable acquisition time for {name}"
                    ))
                })?;
                Ok(Holding {
                    territory_id: territories[name],
                    guild_id: guilds[&legacy_guild_key(record)],
                    acquired_micros: micros(acquired),
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        holdings.sort_unstable_by_key(|holding| holding.territory_id);
        sqlx::query(
            "INSERT INTO history_checkpoint (taken_at, through_seq, source, legacy_snapshot_id, \
                 ownership) \
             SELECT $1, coalesce((SELECT stream_seq FROM history_fact WHERE recorded_at <= $1 \
                 ORDER BY recorded_at DESC, stream_seq DESC LIMIT 1), 0), 0, $2, $3 \
             ON CONFLICT (legacy_snapshot_id) DO NOTHING",
        )
        .bind(created_at)
        .bind(id)
        .bind(checkpoint::encode(&holdings))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE history_meta SET legacy_snapshots_through = $1")
        .bind(last_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(parsed.len() as u64)
}

fn legacy_guild_key(record: &LegacyOwnership) -> GuildKey {
    GuildKey {
        uuid: record.guild_uuid.clone(),
        name: record.guild_name.clone(),
        prefix: record.guild_prefix.clone(),
        color: pack_color(record.guild_color),
    }
}

type GapFact = (i64, DateTime<Utc>, i16, i32, DateTime<Utc>);

/// Split every checkpoint interval holding more facts than the replay budget with derived
/// checkpoints. A derived checkpoint at `T` folds exactly the facts a seek would replay up
/// to `T`, and `T` is chosen only where no later-recorded fact precedes an earlier one in
/// stream order, so seeks after it return what the legacy reconstruction returned.
pub async fn derive_checkpoints(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    let budget = i64::from(CHECKPOINT_FACT_BUDGET);
    let mut bases: Vec<(Option<DateTime<Utc>>, Option<i64>)> = vec![(None, None)];
    bases.extend(
        sqlx::query_as::<_, (DateTime<Utc>, i64)>(
            "SELECT taken_at, id FROM history_checkpoint ORDER BY taken_at, id",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(taken_at, id)| (Some(taken_at), Some(id))),
    );
    let mut derived = 0;
    for window in 0..bases.len() {
        let (since, base_id) = bases[window];
        let until = bases.get(window + 1).and_then(|(taken_at, _)| *taken_at);
        let since_bound = since.unwrap_or(super::beginning());
        let until_bound = until.unwrap_or(super::end_of_time());
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM history_fact WHERE recorded_at > $1 AND recorded_at < $2",
        )
        .bind(since_bound)
        .bind(until_bound)
        .fetch_one(pool)
        .await?;
        if count <= budget {
            continue;
        }
        let facts: Vec<GapFact> = sqlx::query_as(
            "SELECT stream_seq, recorded_at, territory_id, guild_id, acquired_at FROM history_fact \
             WHERE recorded_at > $1 AND recorded_at < $2 ORDER BY stream_seq, ordinal",
        )
        .bind(since_bound)
        .bind(until_bound)
        .fetch_all(pool)
        .await?;
        let base: Vec<Holding> = match base_id {
            Some(id) => {
                let bytes: Vec<u8> =
                    sqlx::query_scalar("SELECT ownership FROM history_checkpoint WHERE id = $1")
                        .bind(id)
                        .fetch_one(pool)
                        .await?;
                checkpoint::decode(&bytes)
                    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
            }
            None => Vec::new(),
        };
        for (taken_at, through_seq, holdings) in split_points(&base, &facts, budget as usize) {
            sqlx::query(
                "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership) \
                 VALUES ($1, $2, 1, $3)",
            )
            .bind(taken_at)
            .bind(through_seq)
            .bind(checkpoint::encode(&holdings))
            .execute(pool)
            .await?;
            derived += 1;
        }
    }
    Ok(derived)
}

/// Checkpoints inside one interval: about every `budget` facts by recorded time, at times
/// that separate the facts cleanly in both time and stream order.
fn split_points(
    base: &[Holding],
    facts: &[GapFact],
    budget: usize,
) -> Vec<(DateTime<Utc>, i64, Vec<Holding>)> {
    let mut by_time: Vec<usize> = (0..facts.len()).collect();
    by_time.sort_by_key(|&index| (facts[index].1, index));
    // prefix_max_seq[k]: highest stream position among the first k facts by time;
    // suffix_min_seq[k]: lowest among the rest.
    let mut prefix_max_seq = vec![i64::MIN; facts.len() + 1];
    for (k, &index) in by_time.iter().enumerate() {
        prefix_max_seq[k + 1] = prefix_max_seq[k].max(facts[index].0);
    }
    let mut suffix_min_seq = vec![i64::MAX; facts.len() + 1];
    for k in (0..facts.len()).rev() {
        suffix_min_seq[k] = suffix_min_seq[k + 1].min(facts[by_time[k]].0);
    }

    let mut points = Vec::new();
    let mut last_cut = 0;
    let mut k = budget;
    while k < facts.len() {
        let at = facts[by_time[k - 1]].1;
        let clean = facts[by_time[k]].1 > at && prefix_max_seq[k] < suffix_min_seq[k];
        if clean && k - last_cut >= budget / 2 {
            let mut owners: HashMap<i16, Holding> = base
                .iter()
                .map(|holding| (holding.territory_id, *holding))
                .collect();
            for &(_, recorded_at, territory_id, guild_id, acquired_at) in facts {
                if recorded_at <= at {
                    owners.insert(
                        territory_id,
                        Holding {
                            territory_id,
                            guild_id,
                            acquired_micros: micros(acquired_at),
                        },
                    );
                }
            }
            let mut holdings: Vec<Holding> = owners.into_values().collect();
            holdings.sort_unstable_by_key(|holding| holding.territory_id);
            points.push((at, prefix_max_seq[k], holdings));
            last_cut = k;
            k += budget;
        } else {
            k += 1;
        }
    }
    points
}

/// Convert legacy archive rows above the watermark in id order, keeping their ids.
pub async fn convert_archive(pool: &sqlx::PgPool) -> Result<ConvertReport, sqlx::Error> {
    if !super::table_exists(pool, "canonical_territory_updates").await? {
        return Ok(ConvertReport::default());
    }
    type ArchiveRow = (
        i64,
        DateTime<Utc>,
        String,
        DateTime<Utc>,
        f64,
        String,
        String,
        i32,
        Option<String>,
        Value,
    );
    let mut report = ConvertReport::default();
    loop {
        let mut tx = pool.begin().await?;
        let done: i64 = sqlx::query_scalar("SELECT legacy_archive_through FROM history_meta")
            .fetch_one(&mut *tx)
            .await?;
        let rows: Vec<ArchiveRow> = sqlx::query_as(
            "SELECT id, received_at, territory, observed_at, confidence, visibility, source, \
                 reporter_count, idempotency_key, payload \
             FROM canonical_territory_updates WHERE id > $1 ORDER BY id LIMIT $2",
        )
        .bind(done)
        .bind(ARCHIVE_CHUNK)
        .fetch_all(&mut *tx)
        .await?;
        let Some(&(last_id, ..)) = rows.last() else {
            break;
        };
        let reports: Vec<Report> = rows
            .into_iter()
            .map(
                |(
                    id,
                    received_at,
                    territory,
                    observed_at,
                    confidence,
                    visibility,
                    source,
                    reporter_count,
                    idempotency_key,
                    payload,
                )| {
                    Report::from_payload(
                        payload,
                        Columns {
                            id: Some(id),
                            territory,
                            received_at,
                            observed_at,
                            confidence,
                            visibility: &visibility,
                            source,
                            reporter_count,
                            idempotency_key,
                        },
                    )
                },
            )
            .collect();
        let mut names: Vec<String> = reports.iter().map(|r| r.territory.clone()).collect();
        names.sort_unstable();
        names.dedup();
        let mut keys: Vec<GuildKey> = reports.iter().filter_map(|r| r.guild().cloned()).collect();
        keys.sort_unstable_by(|a, b| {
            (&a.uuid, &a.name, &a.prefix, a.color).cmp(&(&b.uuid, &b.name, &b.prefix, b.color))
        });
        keys.dedup();
        let territories = insert_territories(&mut tx, &names).await?;
        let guilds = insert_guilds(&mut tx, &keys).await?;
        let resolved: Vec<archive::Resolved> = reports
            .iter()
            .map(|report| archive::Resolved {
                report,
                territory_id: territories[&report.territory],
                guild_id: report.guild().map(|key| guilds[key]),
            })
            .collect();
        archive::insert(&mut tx, &resolved).await?;
        sqlx::query("UPDATE history_meta SET legacy_archive_through = $1")
            .bind(last_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        report.archive_rows += reports.len() as u64;
        report.archive_raw_rows += reports
            .iter()
            .filter(|r| matches!(r.body, archive::Body::Raw(_)))
            .count() as u64;
        info!(through = last_id, "converted legacy archive rows");
    }
    Ok(report)
}

#[derive(Debug, Default, serde::Serialize)]
pub struct VerifyReport {
    pub legacy_events: i64,
    pub events_without_exact_fact: i64,
    pub legacy_snapshots: i64,
    pub snapshots_without_checkpoint: i64,
    pub sampled_instants: usize,
    pub mismatched_instants: Vec<String>,
    pub legacy_archive_rows: i64,
    /// Legacy archive rows with no converted report: never converted, or lost since.
    pub archive_missing: i64,
    /// Converted reports whose columns or rebuilt payload differ from their source.
    pub archive_mismatched: i64,
    /// The first of each kind, by id.
    pub archive_missing_ids: Vec<i64>,
    pub archive_mismatched_ids: Vec<i64>,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.events_without_exact_fact == 0
            && self.snapshots_without_checkpoint == 0
            && self.mismatched_instants.is_empty()
            && self.archive_missing == 0
            && self.archive_mismatched == 0
    }
}

const LISTED_IDS: usize = 100;

/// Compare every converted legacy event field, the reconstructed ownership at `instants`
/// against the legacy reconstruction, and every converted archive row against its source.
pub async fn verify(
    pool: &sqlx::PgPool,
    instants: &[DateTime<Utc>],
) -> Result<VerifyReport, sqlx::Error> {
    let (legacy_events, events_without_exact_fact): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE NOT EXISTS ( \
             SELECT 1 FROM history_fact f \
             JOIN history_territory t ON t.id = f.territory_id \
             JOIN history_guild g ON g.id = f.guild_id \
             LEFT JOIN history_guild p ON p.id = f.prev_guild_id \
             WHERE f.stream_seq = e.stream_seq AND f.ordinal = 0 \
               AND f.recorded_at = e.recorded_at AND f.acquired_at = e.acquired_at \
               AND t.name = e.territory AND g.uuid = e.guild_uuid AND g.name = e.guild_name \
               AND g.prefix = e.guild_prefix \
               AND g.color IS NOT DISTINCT FROM ((e.guild_color_r::int << 16) \
                   | (e.guild_color_g::int << 8) | e.guild_color_b::int) \
               AND p.uuid IS NOT DISTINCT FROM e.prev_guild_uuid \
               AND p.name IS NOT DISTINCT FROM e.prev_guild_name \
               AND p.prefix IS NOT DISTINCT FROM e.prev_guild_prefix \
               AND p.color IS NOT DISTINCT FROM ((e.prev_guild_color_r::int << 16) \
                   | (e.prev_guild_color_g::int << 8) | e.prev_guild_color_b::int))) \
         FROM territory_events e",
    )
    .fetch_one(pool)
    .await?;

    let mut mismatched_instants = Vec::new();
    for &at in instants {
        let legacy = legacy_ownership_at(pool, at).await?;
        let current = super::read::ownership_at(pool, at)
            .await
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let current: HashMap<String, (String, String, String, Option<i32>, i64)> = current
            .into_iter()
            .map(|(name, record)| {
                (
                    name,
                    (
                        record.guild_uuid,
                        record.guild_name,
                        record.guild_prefix,
                        pack_color(record.guild_color),
                        parse_time(&record.acquired_at).map_or(i64::MIN, micros),
                    ),
                )
            })
            .collect();
        if legacy != current {
            mismatched_instants.push(at.to_rfc3339());
        }
    }

    let (legacy_snapshots, snapshots_without_checkpoint): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE NOT EXISTS ( \
             SELECT 1 FROM history_checkpoint c WHERE c.legacy_snapshot_id = s.id \
               AND c.taken_at = s.created_at)) \
         FROM territory_snapshots s",
    )
    .fetch_one(pool)
    .await?;

    let mut report = VerifyReport {
        legacy_events,
        events_without_exact_fact,
        legacy_snapshots,
        snapshots_without_checkpoint,
        sampled_instants: instants.len(),
        mismatched_instants,
        ..VerifyReport::default()
    };
    let mut after = 0_i64;
    loop {
        // Every legacy row, with its converted report when there is one.
        type Pair = (
            i64,
            Option<String>,
            Option<String>,
            Option<DateTime<Utc>>,
            Option<Vec<u8>>,
            Option<Value>,
            Option<Value>,
            Option<Value>,
            Value,
            Option<bool>,
            bool,
        );
        let rows: Vec<Pair> = sqlx::query_as(
            "SELECT l.id, t.name, r.idempotency_key, r.acquired_at, r.runtime, r.raw_payload, \
                 s.fields, \
                 CASE WHEN g.id IS NOT NULL \
                     THEN jsonb_build_array(g.uuid, g.name, g.prefix, g.color) END, \
                 l.payload, \
                 (l.received_at, l.observed_at, l.confidence, l.reporter_count, l.territory, \
                  l.idempotency_key, l.source) \
                 IS NOT DISTINCT FROM (r.received_at, r.observed_at, r.confidence, \
                  r.reporter_count, t.name, r.idempotency_key, src.name) \
                 AND l.visibility IS NOT DISTINCT FROM CASE r.visibility \
                     WHEN 0 THEN 'public' WHEN 1 THEN 'guild_opt_in' END, \
                 r.id IS NOT NULL \
             FROM canonical_territory_updates l \
             LEFT JOIN canonical_report r ON r.id = l.id \
             LEFT JOIN history_territory t ON t.id = r.territory_id \
             LEFT JOIN canonical_source src ON src.id = r.source_id \
             LEFT JOIN canonical_static s ON s.id = r.static_id \
             LEFT JOIN history_guild g ON g.id = r.guild_id \
             WHERE l.id > $1 ORDER BY l.id LIMIT 20000",
        )
        .bind(after)
        .fetch_all(pool)
        .await?;
        let Some(&(last, ..)) = rows.last() else {
            break;
        };
        after = last;
        for (
            id,
            territory,
            key,
            acquired_at,
            runtime,
            raw,
            statics,
            guild,
            legacy,
            columns_equal,
            converted,
        ) in rows
        {
            report.legacy_archive_rows += 1;
            if !converted {
                report.archive_missing += 1;
                if report.archive_missing_ids.len() < LISTED_IDS {
                    report.archive_missing_ids.push(id);
                }
                continue;
            }
            let guild = guild
                .and_then(|value| {
                    serde_json::from_value::<(String, String, String, Option<i32>)>(value).ok()
                })
                .map(|(uuid, name, prefix, color)| GuildKey {
                    uuid,
                    name,
                    prefix,
                    color,
                });
            let rebuilt = match raw {
                Some(raw) => Some(raw),
                None => archive::rebuild(
                    territory.as_deref().unwrap_or_default(),
                    key.as_deref(),
                    &archive::Body::Compact {
                        guild,
                        acquired_at,
                        statics,
                        runtime,
                    },
                )
                .and_then(|update| serde_json::to_value(update).ok()),
            };
            if columns_equal != Some(true) || rebuilt.as_ref() != Some(&legacy) {
                report.archive_mismatched += 1;
                if report.archive_mismatched_ids.len() < LISTED_IDS {
                    report.archive_mismatched_ids.push(id);
                }
            }
        }
    }
    Ok(report)
}

type LegacyEventRow = (
    String,
    String,
    String,
    String,
    Option<i16>,
    Option<i16>,
    Option<i16>,
    DateTime<Utc>,
);

/// The legacy reconstruction without presentation colour fallbacks: latest snapshot at or
/// before `at`, then events recorded after it in stream order.
async fn legacy_ownership_at(
    pool: &sqlx::PgPool,
    at: DateTime<Utc>,
) -> Result<HashMap<String, (String, String, String, Option<i32>, i64)>, sqlx::Error> {
    let snapshot: Option<(DateTime<Utc>, Value)> = sqlx::query_as(
        "SELECT created_at, ownership FROM territory_snapshots WHERE created_at <= $1 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(at)
    .fetch_optional(pool)
    .await?;
    let mut owners = HashMap::new();
    let since = match snapshot {
        Some((created_at, ownership)) => {
            let ownership: HashMap<String, LegacyOwnership> = serde_json::from_value(ownership)
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
            for (name, record) in ownership {
                let acquired = parse_time(&record.acquired_at).map_or(i64::MIN, micros);
                owners.insert(
                    name,
                    (
                        record.guild_uuid,
                        record.guild_name,
                        record.guild_prefix,
                        pack_color(record.guild_color),
                        acquired,
                    ),
                );
            }
            created_at
        }
        None => super::beginning(),
    };
    let events: Vec<LegacyEventRow> = sqlx::query_as(
        "SELECT territory, guild_uuid, guild_name, guild_prefix, guild_color_r, guild_color_g, \
             guild_color_b, acquired_at FROM territory_events \
         WHERE recorded_at > $1 AND recorded_at <= $2 ORDER BY stream_seq",
    )
    .bind(since)
    .bind(at)
    .fetch_all(pool)
    .await?;
    for (territory, uuid, name, prefix, r, g, b, acquired_at) in events {
        let color = match (r, g, b) {
            (Some(r), Some(g), Some(b)) => {
                Some((i32::from(r) << 16) | (i32::from(g) << 8) | i32::from(b))
            }
            _ => None,
        };
        owners.insert(territory, (uuid, name, prefix, color, micros(acquired_at)));
    }
    Ok(owners)
}

#[derive(Debug, Default, serde::Serialize)]
pub struct RollbackReport {
    pub events: u64,
    /// Facts sharing a stream position with an earlier fact of the same canonical batch;
    /// the legacy schema keeps one row per position and never recorded these.
    pub batch_facts_not_representable: u64,
    pub snapshots: u64,
    pub archive_rows: u64,
}

/// Copy facts, writer checkpoints and archive reports recorded after the legacy tables'
/// last rows back into them. Rerunning copies only what is still missing.
pub async fn rollback(pool: &sqlx::PgPool) -> Result<RollbackReport, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let events: i64 = sqlx::query_scalar(
        "WITH inserted AS ( \
             INSERT INTO territory_events (stream_seq, recorded_at, acquired_at, territory, \
                 guild_uuid, guild_name, guild_prefix, guild_color_r, guild_color_g, \
                 guild_color_b, prev_guild_uuid, prev_guild_name, prev_guild_prefix, \
                 prev_guild_color_r, prev_guild_color_g, prev_guild_color_b) \
             SELECT f.stream_seq, f.recorded_at, f.acquired_at, t.name, g.uuid, g.name, \
                 g.prefix, (g.color >> 16) & 255, (g.color >> 8) & 255, g.color & 255, \
                 p.uuid, p.name, p.prefix, (p.color >> 16) & 255, (p.color >> 8) & 255, \
                 p.color & 255 \
             FROM history_fact f \
             JOIN history_territory t ON t.id = f.territory_id \
             JOIN history_guild g ON g.id = f.guild_id \
             LEFT JOIN history_guild p ON p.id = f.prev_guild_id \
             WHERE f.ordinal = 0 AND f.stream_seq > \
                 coalesce((SELECT max(stream_seq) FROM territory_events), 0) \
             ORDER BY f.stream_seq \
             RETURNING 1 \
         ) SELECT count(*) FROM inserted",
    )
    .fetch_one(&mut *tx)
    .await?;
    let batch_facts_not_representable: i64 =
        sqlx::query_scalar("SELECT count(*) FROM history_fact WHERE ordinal > 0 AND source = 2")
            .fetch_one(&mut *tx)
            .await?;

    let checkpoints: Vec<(DateTime<Utc>, Vec<u8>)> = sqlx::query_as(
        "SELECT taken_at, ownership FROM history_checkpoint WHERE source = 2 AND taken_at > \
             coalesce((SELECT max(created_at) FROM territory_snapshots), '-infinity') \
         ORDER BY taken_at",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (taken_at, bytes) in &checkpoints {
        let holdings =
            checkpoint::decode(bytes).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let territory_ids: Vec<i16> = holdings.iter().map(|h| h.territory_id).collect();
        let guild_ids: Vec<i32> = holdings.iter().map(|h| h.guild_id).collect();
        let names: HashMap<i16, String> =
            sqlx::query_as("SELECT id, name FROM history_territory WHERE id = ANY($1)")
                .bind(&territory_ids)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect();
        let guilds: HashMap<i32, (String, String, String, Option<i32>)> =
            sqlx::query_as::<_, (i32, String, String, String, Option<i32>)>(
                "SELECT id, uuid, name, prefix, color FROM history_guild WHERE id = ANY($1)",
            )
            .bind(&guild_ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(id, uuid, name, prefix, color)| (id, (uuid, name, prefix, color)))
            .collect();
        let ownership: serde_json::Map<String, Value> = holdings
            .iter()
            .filter_map(|holding| {
                let (uuid, name, prefix, color) = guilds.get(&holding.guild_id)?;
                let mut record = serde_json::json!({
                    "guild_uuid": uuid,
                    "guild_name": name,
                    "guild_prefix": prefix,
                    "acquired_at": super::from_micros(holding.acquired_micros),
                });
                if let Some(color) = unpack_color(*color) {
                    record["guild_color"] = serde_json::json!(color);
                }
                Some((names.get(&holding.territory_id)?.clone(), record))
            })
            .collect();
        sqlx::query("INSERT INTO territory_snapshots (created_at, ownership) VALUES ($1, $2)")
            .bind(taken_at)
            .bind(Value::Object(ownership))
            .execute(&mut *tx)
            .await?;
    }

    type Row = (
        i64,
        DateTime<Utc>,
        String,
        DateTime<Utc>,
        f64,
        i16,
        String,
        i32,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<Vec<u8>>,
        Option<Value>,
        Option<Value>,
        Option<Value>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT r.id, r.received_at, t.name, r.observed_at, r.confidence, r.visibility, \
             src.name, r.reporter_count, r.idempotency_key, r.acquired_at, r.runtime, \
             r.raw_payload, s.fields, \
             CASE WHEN g.id IS NOT NULL THEN jsonb_build_array(g.uuid, g.name, g.prefix, g.color) END \
         FROM canonical_report r \
         JOIN history_territory t ON t.id = r.territory_id \
         JOIN canonical_source src ON src.id = r.source_id \
         LEFT JOIN canonical_static s ON s.id = r.static_id \
         LEFT JOIN history_guild g ON g.id = r.guild_id \
         WHERE r.id > coalesce((SELECT max(id) FROM canonical_territory_updates), 0) \
         ORDER BY r.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let archive_rows = rows.len();
    for (
        id,
        received_at,
        territory,
        observed_at,
        confidence,
        visibility,
        source,
        reporter_count,
        key,
        acquired_at,
        runtime,
        raw,
        statics,
        guild,
    ) in rows
    {
        let guild = guild
            .map(serde_json::from_value::<(String, String, String, Option<i32>)>)
            .transpose()
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let report = Report {
            id: Some(id),
            territory,
            received_at,
            observed_at,
            confidence,
            visibility,
            source,
            reporter_count,
            idempotency_key: key,
            body: match raw {
                Some(raw) => archive::Body::Raw(raw),
                None => archive::Body::Compact {
                    guild: guild.map(|(uuid, name, prefix, color)| GuildKey {
                        uuid,
                        name,
                        prefix,
                        color,
                    }),
                    acquired_at,
                    statics,
                    runtime,
                },
            },
        };
        let payload = archive::payload(&report).ok_or_else(|| {
            sqlx::Error::Protocol(format!("archive report {id} cannot be rebuilt"))
        })?;
        sqlx::query(
            "INSERT INTO canonical_territory_updates (id, received_at, territory, observed_at, \
                 confidence, visibility, source, reporter_count, idempotency_key, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(received_at)
        .bind(&report.territory)
        .bind(observed_at)
        .bind(confidence)
        .bind(archive::visibility_name(visibility))
        .bind(&report.source)
        .bind(reporter_count)
        .bind(report.idempotency_key.as_deref())
        .bind(payload)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "SELECT setval(pg_get_serial_sequence('canonical_territory_updates', 'id'), \
             (SELECT max(id) FROM canonical_territory_updates)) \
         WHERE EXISTS (SELECT 1 FROM canonical_territory_updates)",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(RollbackReport {
        events: u64::try_from(events).unwrap_or(0),
        batch_facts_not_representable: u64::try_from(batch_facts_not_representable).unwrap_or(0),
        snapshots: checkpoints.len() as u64,
        archive_rows: archive_rows as u64,
    })
}

/// `sequoia-server history <convert|archive|verify [samples]|rollback>`.
pub async fn run_cli(
    pool: &sqlx::PgPool,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let output = match args.first().map(String::as_str) {
        Some("convert") => format!("{:?}", convert_history(pool).await?),
        Some("archive") => format!("{:?}", convert_archive(pool).await?),
        Some("verify") => {
            let samples = args
                .get(1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(2_000);
            let instants = verification_instants(pool, samples).await?;
            let report = verify(pool, &instants).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_clean() {
                return Err("history verification found missing or mismatched rows".into());
            }
            return Ok(());
        }
        Some("rollback") => serde_json::to_string_pretty(&rollback(pool).await?)?,
        _ => {
            return Err(
                "usage: sequoia-server history <convert|archive|verify [samples]|rollback>".into(),
            );
        }
    };
    println!("{output}");
    Ok(())
}

/// Every checkpoint boundary and the instant before it, plus evenly spread instants over the
/// retained range and the instants of the latest facts.
async fn verification_instants(
    pool: &sqlx::PgPool,
    samples: i64,
) -> Result<Vec<DateTime<Utc>>, sqlx::Error> {
    let one = chrono::Duration::microseconds(1);
    let mut instants: Vec<DateTime<Utc>> = Vec::new();
    for taken_at in
        sqlx::query_scalar::<_, DateTime<Utc>>("SELECT taken_at FROM history_checkpoint")
            .fetch_all(pool)
            .await?
    {
        instants.extend([taken_at - one, taken_at, taken_at + one]);
    }
    let bounds = super::read::bounds(pool).await?;
    if let (Some(earliest), Some(latest)) = (bounds.earliest, bounds.latest) {
        let span = (latest - earliest).num_microseconds().unwrap_or(0).max(1);
        for k in 0..=samples.max(1) {
            instants.push(earliest + chrono::Duration::microseconds(span / samples.max(1) * k));
        }
    }
    instants.extend(
        sqlx::query_scalar::<_, DateTime<Utc>>(
            "SELECT recorded_at FROM history_fact ORDER BY stream_seq DESC LIMIT 50",
        )
        .fetch_all(pool)
        .await?,
    );
    instants.sort_unstable();
    instants.dedup();
    Ok(instants)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid time")
    }

    fn cuts(facts: &[(i64, i64)], budget: usize) -> Vec<(i64, i64)> {
        let facts: Vec<GapFact> = facts
            .iter()
            .map(|&(seq, time)| (seq, at(time), seq as i16, 1, at(time)))
            .collect();
        split_points(&[], &facts, budget)
            .iter()
            .map(|(taken_at, through, _)| (taken_at.timestamp(), *through))
            .collect()
    }

    #[test]
    fn derived_checkpoints_never_split_a_time_tie_or_a_sequence_inversion() {
        // Positions 3 and 4 share a time: the cut waits until both are included.
        assert_eq!(
            cuts(&[(1, 10), (2, 20), (3, 30), (4, 30), (5, 50)], 3),
            vec![(30, 4)]
        );
        // Position 3 was recorded after position 4: no cut may separate them.
        assert_eq!(
            cuts(&[(1, 10), (2, 20), (3, 45), (4, 40), (5, 50), (6, 60)], 3),
            vec![(45, 4)]
        );
    }

    async fn test_database() -> Option<(
        sqlx::PgPool,
        sqlx_core::pool::PoolConnection<sqlx::Postgres>,
    )> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect test database");
        let mut lock = pool.acquire().await.expect("lock connection");
        sqlx::query("SELECT pg_advisory_lock(73019001)")
            .execute(&mut *lock)
            .await
            .expect("take test lock");
        crate::db_migrations::run(&pool).await.expect("migrate");
        crate::test_support::reset_history(&pool).await;
        Some((pool, lock))
    }

    async fn identities_drawn(pool: &sqlx::PgPool, table: &str) -> i64 {
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT pg_sequence_last_value(pg_get_serial_sequence($1, 'id')::regclass)",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .expect("sequence position")
        .unwrap_or(0)
    }

    async fn add_legacy_reports(pool: &sqlx::PgPool, territories: &[&str]) {
        for (index, territory) in territories.iter().enumerate() {
            let update = sequoia_shared::CanonicalTerritoryUpdate {
                territory: (*territory).to_string(),
                guild: Some(sequoia_shared::GuildRef {
                    uuid: "u".into(),
                    name: "Guild".into(),
                    prefix: "G".into(),
                    color: Some((1, 2, 3)),
                }),
                acquired: Some("2026-07-20T16:48:22.328+00:00".into()),
                location: None,
                resources: None,
                connections: None,
                runtime: None,
                idempotency_key: Some(format!(
                    "{territory}-{index}-{}",
                    Utc::now().timestamp_nanos_opt().unwrap_or(0)
                )),
            };
            sqlx::query(
                "INSERT INTO canonical_territory_updates (territory, observed_at, confidence, \
                     visibility, source, reporter_count, idempotency_key, payload) \
                 VALUES ($1, now(), 0.5, 'public', 'fabric_reporter', 1, $2, $3)",
            )
            .bind(*territory)
            .bind(update.idempotency_key.clone())
            .bind(serde_json::to_value(&update).expect("payload"))
            .execute(pool)
            .await
            .expect("insert legacy report");
        }
    }

    #[tokio::test]
    async fn repeated_conversion_and_dictionary_inserts_draw_identities_only_for_new_entries() {
        let Some((pool, _lock)) = test_database().await else {
            eprintln!("Skipping dictionary identity test: TEST_DATABASE_URL is not set");
            return;
        };
        add_legacy_reports(&pool, &["Alpha", "Beta", "Alpha"]).await;
        convert_archive(&pool).await.expect("first conversion");
        assert_eq!(identities_drawn(&pool, "history_territory").await, 2);
        assert_eq!(identities_drawn(&pool, "canonical_source").await, 1);
        assert_eq!(identities_drawn(&pool, "history_guild").await, 1);

        // Later chunks and the writer's batches name the same entries again.
        for _ in 0..5 {
            add_legacy_reports(&pool, &["Alpha", "Beta"]).await;
            convert_archive(&pool).await.expect("repeat conversion");
            let mut tx = pool.begin().await.expect("tx");
            let names = vec!["Alpha".to_string(), "Beta".to_string()];
            let ids = super::super::writer::insert_territories(&mut tx, &names)
                .await
                .expect("territories");
            assert_eq!(ids.len(), 2);
            tx.commit().await.expect("commit");
        }
        assert_eq!(identities_drawn(&pool, "history_territory").await, 2);
        assert_eq!(identities_drawn(&pool, "canonical_source").await, 1);
        assert_eq!(identities_drawn(&pool, "history_guild").await, 1);

        add_legacy_reports(&pool, &["Gamma"]).await;
        convert_archive(&pool).await.expect("new territory");
        assert_eq!(identities_drawn(&pool, "history_territory").await, 3);
    }

    #[tokio::test]
    async fn verification_rejects_an_omitted_and_a_corrupted_archive_row() {
        let Some((pool, _lock)) = test_database().await else {
            eprintln!("Skipping archive verification test: TEST_DATABASE_URL is not set");
            return;
        };
        add_legacy_reports(&pool, &["Alpha", "Beta", "Gamma"]).await;
        convert_archive(&pool).await.expect("conversion");
        assert!(verify(&pool, &[]).await.expect("verify").is_clean());

        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM canonical_report ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("report ids");
        sqlx::query("DELETE FROM canonical_report WHERE id = $1")
            .bind(ids[0])
            .execute(&pool)
            .await
            .expect("omit one report");
        sqlx::query("UPDATE canonical_report SET confidence = confidence + 0.25 WHERE id = $1")
            .bind(ids[1])
            .execute(&pool)
            .await
            .expect("corrupt one report");

        let report = verify(&pool, &[]).await.expect("verify");
        assert_eq!(report.archive_missing_ids, vec![ids[0]]);
        assert_eq!(report.archive_mismatched_ids, vec![ids[1]]);
        assert!(!report.is_clean());
        assert!(
            run_cli(&pool, &["verify".to_string(), "1".to_string()])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_entry_another_session_commits_while_this_insert_waits_is_returned() {
        let Some((pool, _lock)) = test_database().await else {
            eprintln!("Skipping dictionary race test: TEST_DATABASE_URL is not set");
            return;
        };
        let names = vec!["Race".to_string()];
        let key = GuildKey {
            uuid: "r".into(),
            name: "Racer".into(),
            prefix: "R".into(),
            color: None,
        };

        let mut first = pool.begin().await.expect("first session");
        let first_territories = insert_territories(&mut first, &names).await.expect("first");
        let first_guilds = insert_guilds(&mut first, std::slice::from_ref(&key))
            .await
            .expect("first guild");

        let second = tokio::spawn({
            let (pool, names, key) = (pool.clone(), names.clone(), key.clone());
            async move {
                let mut tx = pool.begin().await.expect("second session");
                let territories = insert_territories(&mut tx, &names).await.expect("second");
                let guilds = insert_guilds(&mut tx, &[key]).await.expect("second guild");
                tx.commit().await.expect("second commit");
                (territories, guilds)
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !second.is_finished(),
            "the second insert waits for the first session"
        );
        first.commit().await.expect("first commit");

        let (territories, guilds) = second.await.expect("second session completes");
        assert_eq!(territories, first_territories);
        assert_eq!(guilds, first_guilds);
    }
}
