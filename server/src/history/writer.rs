//! The single owner of live ownership changes. The poller and canonical ingest both hold
//! [`HistoryWriter::lock`] while they diff against the live map, take stream positions,
//! commit the resulting facts and publish, so neither can overwrite or reorder the other.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Duration, Utc};
use sequoia_shared::{GuildRef, TerritoryChange, TerritoryMap};
use tokio::sync::{Mutex, MutexGuard};
use tracing::warn;

use super::archive::{self, Report};
use super::checkpoint::{self, Holding};
use super::{CHECKPOINT_FACT_BUDGET, CHECKPOINT_MAX_AGE_SECS, FactSource, micros, pack_color};

/// Stream positions reserved durably ahead of use, so a restart never reuses one.
const SEQ_RESERVATION: u64 = 4_096;
/// Work kept for retry while the database is unavailable, counted as facts plus archive
/// reports with every batch counting at least once; the oldest batches are dropped beyond it.
const MAX_PENDING_ITEMS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct GuildKey {
    pub uuid: String,
    pub name: String,
    pub prefix: String,
    pub color: Option<i32>,
}

impl From<&GuildRef> for GuildKey {
    fn from(guild: &GuildRef) -> Self {
        Self {
            uuid: guild.uuid.clone(),
            name: guild.name.clone(),
            prefix: guild.prefix.clone(),
            color: pack_color(guild.color),
        }
    }
}

struct PendingFact {
    seq: i64,
    ordinal: i16,
    territory: String,
    guild: GuildKey,
    previous: Option<GuildKey>,
    acquired_at: DateTime<Utc>,
    source: FactSource,
}

/// Ownership a checkpoint records, taken from the live map when the batch was committed.
struct Capture {
    through_seq: i64,
    holdings: Vec<(String, GuildKey, i64)>,
}

struct PendingBatch {
    recorded_at: DateTime<Utc>,
    facts: Vec<PendingFact>,
    reports: Vec<Report>,
    checkpoint: Option<Capture>,
}

impl PendingBatch {
    fn items(&self) -> usize {
        let held = self
            .checkpoint
            .as_ref()
            .map_or(0, |capture| capture.holdings.len());
        (self.facts.len() + self.reports.len() + held).max(1)
    }
}

/// One accepted change of the live map, ready to be made durable.
pub struct Commit<'a> {
    pub source: FactSource,
    /// Ownership changes with their stream position and order within it.
    pub changes: Vec<(u64, i16, &'a TerritoryChange)>,
    pub reports: Vec<Report>,
    /// The live map after this change, for a checkpoint when one is due.
    pub live: &'a TerritoryMap,
    pub territory_set_changed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Durable {
        facts: usize,
    },
    /// Kept in memory and retried, in order, before the next commit. The oldest batches are
    /// dropped, and counted, when the retained work exceeds its bound.
    Deferred {
        pending_facts: usize,
        pending_reports: usize,
        dropped_facts: usize,
        dropped_reports: usize,
    },
    NoDatabase,
}

pub struct HistoryWriter {
    state: Mutex<WriterState>,
}

impl Default for HistoryWriter {
    fn default() -> Self {
        Self {
            state: Mutex::new(WriterState::default()),
        }
    }
}

impl HistoryWriter {
    pub async fn lock(&self) -> MutexGuard<'_, WriterState> {
        self.state.lock().await
    }
}

#[derive(Default)]
pub struct WriterState {
    last_seq: u64,
    reserved_through: u64,
    last_recorded: Option<DateTime<Utc>>,
    /// Facts committed since the last checkpoint capture, stored or still pending.
    facts_since_capture: u32,
    last_capture_at: Option<DateTime<Utc>>,
    territories: HashMap<String, i16>,
    guilds: HashMap<GuildKey, i32>,
    pending: VecDeque<PendingBatch>,
    pending_facts: usize,
    pending_reports: usize,
    pending_items: usize,
}

impl WriterState {
    /// Load the durable position and dictionaries. The stream continues above every position
    /// that may have been published: reserved, stored as a fact, or stored by the legacy table.
    pub async fn recover(&mut self, pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
        let (reserved, latest_fact_seq, latest_at): (i64, Option<i64>, Option<DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT seq_reserved_through, latest_fact_seq, latest_at FROM history_meta",
            )
            .fetch_one(pool)
            .await?;
        let legacy_seq: Option<i64> = if super::table_exists(pool, "territory_events").await? {
            sqlx::query_scalar("SELECT max(stream_seq) FROM territory_events")
                .fetch_one(pool)
                .await?
        } else {
            None
        };
        let checkpoint: Option<(DateTime<Utc>, i64)> = sqlx::query_as(
            "SELECT taken_at, through_seq FROM history_checkpoint \
             ORDER BY taken_at DESC, id DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        let since_checkpoint: i64 =
            sqlx::query_scalar("SELECT count(*) FROM history_fact WHERE recorded_at > $1")
                .bind(checkpoint.map_or(super::beginning(), |(taken_at, _)| taken_at))
                .fetch_one(pool)
                .await?;

        self.territories =
            sqlx::query_as::<_, (String, i16)>("SELECT name, id FROM history_territory")
                .fetch_all(pool)
                .await?
                .into_iter()
                .collect();
        self.guilds = sqlx::query_as::<_, (String, String, String, Option<i32>, i32)>(
            "SELECT uuid, name, prefix, color, id FROM history_guild",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(uuid, name, prefix, color, id)| {
            (
                GuildKey {
                    uuid,
                    name,
                    prefix,
                    color,
                },
                id,
            )
        })
        .collect();

        let highest = [Some(reserved), latest_fact_seq, legacy_seq]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(0);
        self.last_seq = u64::try_from(highest).unwrap_or(0);
        self.reserved_through = self.last_seq;
        self.last_recorded = latest_at;
        self.last_capture_at = checkpoint.map(|(taken_at, _)| taken_at);
        self.facts_since_capture = u32::try_from(since_checkpoint).unwrap_or(u32::MAX);
        Ok(self.last_seq)
    }

    /// Make sure the next `count` positions above both this writer's last one and `published`
    /// are reserved durably, so they may be published. Without a database there is nothing
    /// to reserve; with one that cannot be reached, nothing may be published.
    pub async fn reserve(
        &mut self,
        pool: Option<&sqlx::PgPool>,
        published: u64,
        count: u64,
    ) -> bool {
        let Some(pool) = pool else {
            return true;
        };
        let Some(last) = self.last_seq.max(published).checked_add(count) else {
            return false;
        };
        if last <= self.reserved_through {
            return true;
        }
        let through = last.saturating_add(SEQ_RESERVATION);
        match sqlx::query(
            "UPDATE history_meta SET seq_reserved_through = GREATEST(seq_reserved_through, $1)",
        )
        .bind(i64::try_from(through).unwrap_or(i64::MAX))
        .execute(pool)
        .await
        {
            Ok(_) => {
                self.reserved_through = through;
                true
            }
            Err(error) => {
                warn!(%error, "could not reserve stream positions durably");
                false
            }
        }
    }

    /// Treat positions up to `through` as already reserved, as an earlier run would have.
    #[cfg(test)]
    pub fn assume_reserved(&mut self, through: u64) {
        self.reserved_through = through;
    }

    /// Hand out `count` reserved stream positions above both this writer's last one and
    /// `published`, returning the one before the first. With a database, positions beyond
    /// the durable reservation are refused: a restart could hand them out again.
    pub fn allocate(&mut self, with_database: bool, published: u64, count: u64) -> Option<u64> {
        let base = self.last_seq.max(published);
        let last = base.checked_add(count)?;
        if with_database && last > self.reserved_through {
            return None;
        }
        self.last_seq = last;
        Some(base)
    }

    fn next_recorded_at(&mut self) -> DateTime<Utc> {
        let now =
            DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap_or_else(Utc::now);
        let recorded_at = match self.last_recorded {
            Some(last) if now <= last => last + Duration::microseconds(1),
            _ => now,
        };
        self.last_recorded = Some(recorded_at);
        recorded_at
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub async fn commit(
        &mut self,
        pool: Option<&sqlx::PgPool>,
        commit: Commit<'_>,
    ) -> CommitOutcome {
        let Some(pool) = pool else {
            return CommitOutcome::NoDatabase;
        };
        if commit.changes.is_empty() && commit.reports.is_empty() && !commit.territory_set_changed {
            return self.retry(pool).await;
        }

        let recorded_at = self.next_recorded_at();
        let facts: Vec<PendingFact> = commit
            .changes
            .iter()
            .map(|&(seq, ordinal, change)| PendingFact {
                seq: i64::try_from(seq).unwrap_or(i64::MAX),
                ordinal,
                territory: change.territory.clone(),
                guild: GuildKey::from(&change.guild),
                previous: change.previous_guild.as_ref().map(GuildKey::from),
                acquired_at: parse_acquired(change, recorded_at),
                source: commit.source,
            })
            .collect();
        let added = u32::try_from(facts.len()).unwrap_or(u32::MAX);
        let checkpoint_due = commit.territory_set_changed
            || self.last_capture_at.is_none()
            || self.facts_since_capture.saturating_add(added) >= CHECKPOINT_FACT_BUDGET
            || (added > 0
                && self.last_capture_at.is_some_and(|at| {
                    recorded_at - at >= Duration::seconds(CHECKPOINT_MAX_AGE_SECS)
                }));
        let checkpoint = checkpoint_due.then(|| Capture {
            through_seq: facts
                .iter()
                .map(|fact| fact.seq)
                .max()
                .unwrap_or_else(|| i64::try_from(self.last_seq).unwrap_or(i64::MAX)),
            holdings: commit
                .live
                .iter()
                .map(|(name, territory)| {
                    (
                        name.clone(),
                        GuildKey::from(&territory.guild),
                        micros(territory.acquired),
                    )
                })
                .collect(),
        });
        if checkpoint_due {
            self.facts_since_capture = 0;
            self.last_capture_at = Some(recorded_at);
        } else {
            self.facts_since_capture = self.facts_since_capture.saturating_add(added);
        }
        let batch = PendingBatch {
            recorded_at,
            facts,
            reports: commit.reports,
            checkpoint,
        };
        self.pending_facts += batch.facts.len();
        self.pending_reports += batch.reports.len();
        self.pending_items += batch.items();
        self.pending.push_back(batch);
        self.retry(pool).await
    }

    /// Commit whatever is pending, oldest first; on failure keep it, within its bound.
    pub async fn retry(&mut self, pool: &sqlx::PgPool) -> CommitOutcome {
        if self.pending.is_empty() {
            return CommitOutcome::Durable { facts: 0 };
        }
        let pending_facts = self.pending_facts;
        match self.flush(pool).await {
            Ok(()) => {
                self.pending.clear();
                self.pending_facts = 0;
                self.pending_reports = 0;
                self.pending_items = 0;
                CommitOutcome::Durable {
                    facts: pending_facts,
                }
            }
            Err(error) => {
                warn!(%error, pending_facts, "history commit failed; keeping it for retry");
                let (mut dropped_facts, mut dropped_reports) = (0, 0);
                while self.pending_items > MAX_PENDING_ITEMS
                    && let Some(oldest) = self.pending.pop_front()
                {
                    self.pending_items -= oldest.items();
                    self.pending_facts -= oldest.facts.len();
                    self.pending_reports -= oldest.reports.len();
                    dropped_facts += oldest.facts.len();
                    dropped_reports += oldest.reports.len();
                }
                CommitOutcome::Deferred {
                    pending_facts: self.pending_facts,
                    pending_reports: self.pending_reports,
                    dropped_facts,
                    dropped_reports,
                }
            }
        }
    }

    /// Commit every pending batch, oldest first, with dictionary entries, takeover counts,
    /// metadata, archive reports and each batch's checkpoint at its own time, in one
    /// transaction.
    async fn flush(&mut self, pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        let mut territory_names: HashSet<&str> = HashSet::new();
        let mut guild_keys: HashSet<GuildKey> = HashSet::new();
        for batch in &self.pending {
            for fact in &batch.facts {
                territory_names.insert(&fact.territory);
                guild_keys.insert(fact.guild.clone());
                guild_keys.extend(fact.previous.clone());
            }
            for report in &batch.reports {
                territory_names.insert(&report.territory);
                guild_keys.extend(report.guild().cloned());
            }
            for (name, guild, _) in batch.checkpoint.iter().flat_map(|c| &c.holdings) {
                territory_names.insert(name);
                guild_keys.insert(guild.clone());
            }
        }
        let missing_territories: Vec<String> = territory_names
            .into_iter()
            .filter(|name| !self.territories.contains_key(*name))
            .map(str::to_string)
            .collect();
        let missing_guilds: Vec<GuildKey> = guild_keys
            .into_iter()
            .filter(|key| !self.guilds.contains_key(key))
            .collect();

        let mut tx = pool.begin().await?;
        let new_territories = insert_territories(&mut tx, &missing_territories).await?;
        let new_guilds = insert_guilds(&mut tx, &missing_guilds).await?;
        let territory_id = |name: &str| {
            self.territories
                .get(name)
                .or_else(|| new_territories.get(name))
                .copied()
        };
        let guild_id = |key: &GuildKey| {
            self.guilds
                .get(key)
                .or_else(|| new_guilds.get(key))
                .copied()
        };

        let mut columns = FactColumns::default();
        let mut reports = Vec::new();
        for batch in &self.pending {
            for fact in &batch.facts {
                let (Some(territory), Some(guild)) =
                    (territory_id(&fact.territory), guild_id(&fact.guild))
                else {
                    return Err(sqlx::Error::Protocol(format!(
                        "history dictionary is missing an entry for {}",
                        fact.territory
                    )));
                };
                let previous = fact.previous.as_ref().and_then(guild_id);
                columns.push(FactRow {
                    seq: fact.seq,
                    recorded_at: batch.recorded_at,
                    acquired_at: fact.acquired_at,
                    guild,
                    previous,
                    ordinal: fact.ordinal,
                    territory,
                    source: fact.source as i16,
                    take: fact
                        .previous
                        .as_ref()
                        .is_some_and(|previous| previous.uuid != fact.guild.uuid),
                });
            }
            for report in &batch.reports {
                reports.push(archive::Resolved {
                    report,
                    territory_id: territory_id(&report.territory).unwrap_or_default(),
                    guild_id: report.guild().and_then(guild_id),
                });
            }
        }
        if !columns.seq.is_empty() {
            columns.insert(&mut tx).await?;
        }
        archive::insert(&mut tx, &reports).await?;

        for batch in &self.pending {
            let Some(capture) = &batch.checkpoint else {
                continue;
            };
            let mut holdings: Vec<Holding> = capture
                .holdings
                .iter()
                .filter_map(|(name, guild, acquired_micros)| {
                    Some(Holding {
                        territory_id: territory_id(name)?,
                        guild_id: guild_id(guild)?,
                        acquired_micros: *acquired_micros,
                    })
                })
                .collect();
            holdings.sort_unstable_by_key(|holding| holding.territory_id);
            sqlx::query(
                "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership) \
                 VALUES ($1, $2, 2, $3)",
            )
            .bind(batch.recorded_at)
            .bind(capture.through_seq)
            .bind(checkpoint::encode(&holdings))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        self.territories.extend(new_territories);
        self.guilds.extend(new_guilds);
        Ok(())
    }
}

fn parse_acquired(change: &TerritoryChange, fallback: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&change.acquired)
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_else(|_| {
            warn!(territory = %change.territory, "invalid acquisition time; using commit time");
            fallback
        })
}

/// Dictionary ids for `names`, adding only the names not yet present: an insert that hits an
/// existing entry would still draw a new identity value, and the ids are only 16 bits wide.
///
/// The ids are read by a second statement. Under read committed it sees an entry another
/// session committed while this insert waited on it, which the inserting statement's own
/// snapshot cannot.
pub(crate) async fn insert_territories(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    names: &[String],
) -> Result<HashMap<String, i16>, sqlx::Error> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    sqlx::query(
        "INSERT INTO history_territory (name) \
         SELECT DISTINCT w.name FROM UNNEST($1::text[]) AS w(name) WHERE NOT EXISTS \
             (SELECT 1 FROM history_territory t WHERE t.name = w.name) \
         ORDER BY w.name ON CONFLICT (name) DO NOTHING",
    )
    .bind(names)
    .execute(&mut **tx)
    .await?;
    Ok(sqlx::query_as::<_, (String, i16)>(
        "SELECT name, id FROM history_territory WHERE name = ANY($1)",
    )
    .bind(names)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect())
}

/// Guild dictionary ids for `keys`, with the same allocation and read semantics as
/// [`insert_territories`].
pub(crate) async fn insert_guilds(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    keys: &[GuildKey],
) -> Result<HashMap<GuildKey, i32>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let uuids: Vec<&str> = keys.iter().map(|key| key.uuid.as_str()).collect();
    let names: Vec<&str> = keys.iter().map(|key| key.name.as_str()).collect();
    let prefixes: Vec<&str> = keys.iter().map(|key| key.prefix.as_str()).collect();
    let colors: Vec<Option<i32>> = keys.iter().map(|key| key.color).collect();
    const WANTED: &str = "UNNEST($1::text[], $2::text[], $3::text[], $4::int4[]) \
                          AS w(uuid, name, prefix, color)";
    const SAME: &str = "g.uuid = w.uuid AND g.name = w.name AND g.prefix = w.prefix \
                        AND g.color IS NOT DISTINCT FROM w.color";
    sqlx::query(&format!(
        "INSERT INTO history_guild (uuid, name, prefix, color) \
         SELECT DISTINCT * FROM {WANTED} \
         WHERE NOT EXISTS (SELECT 1 FROM history_guild g WHERE {SAME}) \
         ON CONFLICT (uuid, name, prefix, color) DO NOTHING"
    ))
    .bind(&uuids)
    .bind(&names)
    .bind(&prefixes)
    .bind(&colors)
    .execute(&mut **tx)
    .await?;
    Ok(
        sqlx::query_as::<_, (String, String, String, Option<i32>, i32)>(&format!(
            "SELECT DISTINCT g.uuid, g.name, g.prefix, g.color, g.id \
             FROM history_guild g JOIN {WANTED} ON {SAME}"
        ))
        .bind(&uuids)
        .bind(&names)
        .bind(&prefixes)
        .bind(&colors)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|(uuid, name, prefix, color, id)| {
            (
                GuildKey {
                    uuid,
                    name,
                    prefix,
                    color,
                },
                id,
            )
        })
        .collect(),
    )
}

struct FactRow {
    seq: i64,
    recorded_at: DateTime<Utc>,
    acquired_at: DateTime<Utc>,
    guild: i32,
    previous: Option<i32>,
    ordinal: i16,
    territory: i16,
    source: i16,
    take: bool,
}

#[derive(Default)]
struct FactColumns {
    seq: Vec<i64>,
    recorded_at: Vec<DateTime<Utc>>,
    acquired_at: Vec<DateTime<Utc>>,
    guild: Vec<i32>,
    previous: Vec<Option<i32>>,
    ordinal: Vec<i16>,
    territory: Vec<i16>,
    source: Vec<i16>,
    take: Vec<bool>,
}

impl FactColumns {
    fn push(&mut self, row: FactRow) {
        self.seq.push(row.seq);
        self.recorded_at.push(row.recorded_at);
        self.acquired_at.push(row.acquired_at);
        self.guild.push(row.guild);
        self.previous.push(row.previous);
        self.ordinal.push(row.ordinal);
        self.territory.push(row.territory);
        self.source.push(row.source);
        self.take.push(row.take);
    }

    /// Insert facts and account for exactly the rows that were new, so a retried batch whose
    /// earlier commit did land cannot be counted twice.
    async fn insert(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "WITH inserted AS ( \
                 INSERT INTO history_fact (stream_seq, recorded_at, acquired_at, guild_id, \
                     prev_guild_id, ordinal, territory_id, source, take) \
                 SELECT * FROM UNNEST($1::int8[], $2::timestamptz[], $3::timestamptz[], \
                     $4::int4[], $5::int4[], $6::int2[], $7::int2[], $8::int2[], $9::bool[]) \
                 ON CONFLICT DO NOTHING \
                 RETURNING stream_seq, recorded_at, territory_id, take \
             ), heat AS ( \
                 INSERT INTO history_heat_day (day, territory_id, takes) \
                 SELECT (recorded_at AT TIME ZONE 'UTC')::date, territory_id, count(*) \
                 FROM inserted WHERE take GROUP BY 1, 2 \
                 ON CONFLICT (day, territory_id) \
                 DO UPDATE SET takes = history_heat_day.takes + EXCLUDED.takes \
             ) \
             UPDATE history_meta SET \
                 fact_count = fact_count + (SELECT count(*) FROM inserted), \
                 earliest_at = LEAST(earliest_at, (SELECT min(recorded_at) FROM inserted)), \
                 latest_at = GREATEST(latest_at, (SELECT max(recorded_at) FROM inserted)), \
                 earliest_take_at = LEAST(earliest_take_at, \
                     (SELECT min(recorded_at) FROM inserted WHERE take)), \
                 latest_fact_seq = GREATEST(latest_fact_seq, \
                     (SELECT max(stream_seq) FROM inserted))",
        )
        .bind(self.seq)
        .bind(self.recorded_at)
        .bind(self.acquired_at)
        .bind(self.guild)
        .bind(self.previous)
        .bind(self.ordinal)
        .bind(self.territory)
        .bind(self.source)
        .bind(self.take)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use sequoia_shared::CanonicalTerritoryUpdate;
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    fn unreachable_pool() -> sqlx::PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(StdDuration::from_millis(200))
            .connect_lazy("postgres://sequoia@127.0.0.1:9/sequoia")
            .expect("lazy pool parses")
    }

    fn runtime_reports(count: usize) -> Vec<Report> {
        (0..count)
            .map(|index| {
                Report::from_update(
                    &CanonicalTerritoryUpdate {
                        territory: format!("Territory {index}"),
                        guild: None,
                        acquired: None,
                        location: None,
                        resources: None,
                        connections: None,
                        runtime: None,
                        idempotency_key: Some(format!("key-{index}")),
                    },
                    Utc::now(),
                )
            })
            .collect()
    }

    async fn reset_test_database() -> Option<(
        sqlx::PgPool,
        sqlx_core::pool::PoolConnection<sqlx::Postgres>,
    )> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(3)
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

    #[tokio::test]
    async fn a_removal_during_an_outage_keeps_its_checkpoint_and_a_quiet_commit_retries_it() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping outage checkpoint test: TEST_DATABASE_URL is not set");
            return;
        };
        let held: sequoia_shared::Territory = serde_json::from_value(serde_json::json!({
            "guild": {"uuid": "g", "name": "Guild", "prefix": "G", "color": [1, 2, 3]},
            "acquired": "2026-01-01T00:00:00Z",
            "location": {"start": [0, 0], "end": [10, 10]}
        }))
        .expect("territory");
        let change = |name: &str| TerritoryChange {
            territory: name.to_string(),
            guild: held.guild.clone(),
            previous_guild: None,
            acquired: held.acquired.to_rfc3339(),
            location: held.location.clone(),
            resources: held.resources.clone(),
            connections: Vec::new(),
            runtime: None,
        };
        let commit = |changes, live, territory_set_changed| Commit {
            source: FactSource::Poller,
            changes,
            reports: Vec::new(),
            live,
            territory_set_changed,
        };
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");

        let (fact_a, live_a) = (
            change("A"),
            TerritoryMap::from([("A".to_string(), held.clone())]),
        );
        let outcome = writer
            .commit(Some(&pool), commit(vec![(1, 0, &fact_a)], &live_a, false))
            .await;
        assert_eq!(outcome, CommitOutcome::Durable { facts: 1 });

        let owners = |at| {
            let pool = pool.clone();
            async move {
                let mut names: Vec<String> = super::super::read::ownership_at(&pool, at)
                    .await
                    .expect("seek")
                    .into_keys()
                    .collect();
                names.sort();
                names
            }
        };
        let empty = TerritoryMap::new();

        // A is removed while the database is unreachable; B arrives once it is back, and both
        // are committed together.
        let outcome = writer
            .commit(Some(&unreachable_pool()), commit(Vec::new(), &empty, true))
            .await;
        assert!(matches!(outcome, CommitOutcome::Deferred { .. }));
        let during_removal = Utc::now();
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        let (fact_b, live_b) = (
            change("B"),
            TerritoryMap::from([("B".to_string(), held.clone())]),
        );
        let outcome = writer
            .commit(Some(&pool), commit(vec![(2, 0, &fact_b)], &live_b, false))
            .await;
        assert_eq!(outcome, CommitOutcome::Durable { facts: 1 });
        assert_eq!(owners(during_removal).await, Vec::<String>::new());
        assert_eq!(owners(Utc::now()).await, vec!["B".to_string()]);

        // B is removed during another outage; with the database back and nothing changing,
        // a quiet commit still writes the removal.
        let outcome = writer
            .commit(Some(&unreachable_pool()), commit(Vec::new(), &empty, true))
            .await;
        assert!(matches!(outcome, CommitOutcome::Deferred { .. }));
        let outcome = writer
            .commit(Some(&pool), commit(Vec::new(), &empty, false))
            .await;
        assert_eq!(outcome, CommitOutcome::Durable { facts: 0 });
        assert!(!writer.has_pending());
        assert_eq!(owners(Utc::now()).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn positions_a_database_could_not_reserve_are_never_handed_out_or_reused() {
        let Some((pool, lock)) = reset_test_database().await else {
            eprintln!("Skipping reservation test: TEST_DATABASE_URL is not set");
            return;
        };

        let mut writer = WriterState::default();
        let start = writer.recover(&pool).await.expect("recover");
        // The database is configured but unreachable: nothing may be handed out.
        assert!(!writer.reserve(Some(&unreachable_pool()), start, 5).await);
        assert_eq!(writer.allocate(true, start, 5), None);
        // Without a database the stream runs in memory only, by design.
        assert_eq!(
            WriterState::default().allocate(false, start, 5),
            Some(start)
        );

        assert!(writer.reserve(Some(&pool), start, 5).await);
        let base = writer.allocate(true, start, 5).expect("reserved positions");
        let published = base + 5;

        // A restart before anything else is written continues above every published position.
        let mut restarted = WriterState::default();
        assert!(restarted.recover(&pool).await.expect("recover") >= published);
        drop(lock);
    }

    #[tokio::test]
    async fn an_outage_bounds_queued_reports_and_counts_what_it_drops() {
        let pool = unreachable_pool();
        let live = TerritoryMap::new();
        let mut writer = WriterState::default();
        let (batches, per_batch) = (60, 1_000);
        let mut dropped = 0;
        let mut last = None;
        for _ in 0..batches {
            // Runtime-only accepted reports: no ownership fact accompanies them.
            let outcome = writer
                .commit(
                    Some(&pool),
                    Commit {
                        source: FactSource::Canonical,
                        changes: Vec::new(),
                        reports: runtime_reports(per_batch),
                        live: &live,
                        territory_set_changed: false,
                    },
                )
                .await;
            let CommitOutcome::Deferred {
                pending_reports,
                dropped_reports,
                ..
            } = outcome
            else {
                panic!("an unreachable database defers the commit, got {outcome:?}");
            };
            dropped += dropped_reports;
            last = Some(pending_reports);
        }
        let pending = last.expect("commits ran");
        assert!(writer.pending_items <= MAX_PENDING_ITEMS);
        assert!(pending <= MAX_PENDING_ITEMS);
        assert_eq!(pending + dropped, batches * per_batch);

        // A checkpoint-only batch after a removal still counts against the bound.
        let before = writer.pending_items;
        let outcome = writer
            .commit(
                Some(&pool),
                Commit {
                    source: FactSource::Poller,
                    changes: Vec::new(),
                    reports: Vec::new(),
                    live: &live,
                    territory_set_changed: true,
                },
            )
            .await;
        assert!(matches!(outcome, CommitOutcome::Deferred { .. }));
        assert!(writer.pending_items <= MAX_PENDING_ITEMS);
        assert!(writer.pending_items > before.saturating_sub(per_batch));
    }
}
