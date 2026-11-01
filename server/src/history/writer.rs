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
/// Facts kept for retry while the database is unavailable; older ones are dropped beyond it.
const MAX_PENDING_FACTS: usize = 100_000;

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

struct PendingBatch {
    recorded_at: DateTime<Utc>,
    facts: Vec<PendingFact>,
    reports: Vec<Report>,
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
    /// Kept in memory and retried, in order, before the next commit.
    Deferred {
        pending_facts: usize,
        dropped_facts: usize,
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
    facts_since_checkpoint: u32,
    last_checkpoint_at: Option<DateTime<Utc>>,
    territories: HashMap<String, i16>,
    guilds: HashMap<GuildKey, i32>,
    pending: VecDeque<PendingBatch>,
    pending_facts: usize,
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
        self.last_checkpoint_at = checkpoint.map(|(taken_at, _)| taken_at);
        self.facts_since_checkpoint = u32::try_from(since_checkpoint).unwrap_or(u32::MAX);
        Ok(self.last_seq)
    }

    /// Hand out `count` stream positions above both this writer's last one and `published`,
    /// returning the one before the first. Positions are reserved durably in blocks before
    /// they can be published.
    pub async fn allocate(
        &mut self,
        pool: Option<&sqlx::PgPool>,
        published: u64,
        count: u64,
    ) -> Option<u64> {
        let base = self.last_seq.max(published);
        let last = base.checked_add(count)?;
        if last > self.reserved_through
            && let Some(pool) = pool
        {
            let through = last.saturating_add(SEQ_RESERVATION);
            match sqlx::query(
                "UPDATE history_meta \
                 SET seq_reserved_through = GREATEST(seq_reserved_through, $1)",
            )
            .bind(i64::try_from(through).unwrap_or(i64::MAX))
            .execute(pool)
            .await
            {
                Ok(_) => self.reserved_through = through,
                Err(error) => warn!(%error, "could not reserve stream positions durably"),
            }
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

    pub async fn commit(
        &mut self,
        pool: Option<&sqlx::PgPool>,
        commit: Commit<'_>,
    ) -> CommitOutcome {
        let Some(pool) = pool else {
            return CommitOutcome::NoDatabase;
        };
        if commit.changes.is_empty() && commit.reports.is_empty() && !commit.territory_set_changed {
            return CommitOutcome::Durable { facts: 0 };
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
        self.pending_facts += facts.len();
        self.pending.push_back(PendingBatch {
            recorded_at,
            facts,
            reports: commit.reports,
        });

        let pending_facts = self.pending_facts;
        let checkpoint_due = commit.territory_set_changed
            || self.last_checkpoint_at.is_none()
            || self
                .facts_since_checkpoint
                .saturating_add(u32::try_from(pending_facts).unwrap_or(u32::MAX))
                >= CHECKPOINT_FACT_BUDGET
            || (pending_facts > 0
                && self.last_checkpoint_at.is_some_and(|at| {
                    recorded_at - at >= Duration::seconds(CHECKPOINT_MAX_AGE_SECS)
                }));
        let checkpoint_live = checkpoint_due.then_some(commit.live);

        match self.flush(pool, checkpoint_live, recorded_at).await {
            Ok(()) => {
                self.facts_since_checkpoint = if checkpoint_due {
                    self.last_checkpoint_at = Some(recorded_at);
                    0
                } else {
                    self.facts_since_checkpoint
                        .saturating_add(u32::try_from(pending_facts).unwrap_or(u32::MAX))
                };
                self.pending.clear();
                self.pending_facts = 0;
                CommitOutcome::Durable {
                    facts: pending_facts,
                }
            }
            Err(error) => {
                warn!(%error, pending_facts, "history commit failed; keeping facts for retry");
                let mut dropped_facts = 0;
                while self.pending_facts > MAX_PENDING_FACTS
                    && let Some(oldest) = self.pending.pop_front()
                {
                    self.pending_facts -= oldest.facts.len();
                    dropped_facts += oldest.facts.len();
                }
                CommitOutcome::Deferred {
                    pending_facts: self.pending_facts,
                    dropped_facts,
                }
            }
        }
    }

    /// Commit every pending batch, oldest first, with dictionary entries, takeover counts,
    /// metadata, archive reports and a checkpoint of `live` when given, in one transaction.
    async fn flush(
        &mut self,
        pool: &sqlx::PgPool,
        live: Option<&TerritoryMap>,
        taken_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
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
        }
        if let Some(live) = live {
            for (name, territory) in live {
                territory_names.insert(name);
                guild_keys.insert(GuildKey::from(&territory.guild));
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
        let mut latest_seq = None;
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
                latest_seq = latest_seq.max(Some(fact.seq));
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

        if let Some(live) = live {
            let mut holdings: Vec<Holding> = live
                .iter()
                .filter_map(|(name, territory)| {
                    Some(Holding {
                        territory_id: territory_id(name)?,
                        guild_id: guild_id(&GuildKey::from(&territory.guild))?,
                        acquired_micros: micros(territory.acquired),
                    })
                })
                .collect();
            holdings.sort_unstable_by_key(|holding| holding.territory_id);
            let through_seq = latest_seq.unwrap_or(i64::try_from(self.last_seq).unwrap_or(0));
            sqlx::query(
                "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership) \
                 VALUES ($1, $2, 2, $3)",
            )
            .bind(taken_at)
            .bind(through_seq)
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

pub(crate) async fn insert_territories(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    names: &[String],
) -> Result<HashMap<String, i16>, sqlx::Error> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(sqlx::query_as::<_, (String, i16)>(
        "INSERT INTO history_territory (name) SELECT * FROM UNNEST($1::text[]) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING name, id",
    )
    .bind(names)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect())
}

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
    Ok(
        sqlx::query_as::<_, (String, String, String, Option<i32>, i32)>(
            "INSERT INTO history_guild (uuid, name, prefix, color) \
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::int4[]) \
             ON CONFLICT (uuid, name, prefix, color) DO UPDATE SET uuid = EXCLUDED.uuid \
             RETURNING uuid, name, prefix, color, id",
        )
        .bind(uuids)
        .bind(names)
        .bind(prefixes)
        .bind(colors)
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
