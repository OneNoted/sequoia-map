//! The single owner of live ownership and official-API state changes. The poller and
//! canonical ingest both hold [`HistoryWriter::lock`] while they diff against the live map,
//! take stream positions, commit the resulting facts and publish, so neither can overwrite or
//! reorder the other.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Duration, Utc};
use sequoia_shared::{
    GuildRef, TerritoryChange, TerritoryMap, TerritoryState, TerritoryStateField,
};
use sqlx_core::connection::Connection;
use tokio::sync::{Mutex, MutexGuard};
use tracing::warn;

use super::archive::{self, Report};
use super::checkpoint::{self, Holding, StateRecord};
use super::{
    CHECKPOINT_FACT_BUDGET, CHECKPOINT_MAX_AGE_SECS, CHECKPOINT_STATE_BYTES, FactSource, micros,
    pack_color, state,
};

/// Stream positions reserved durably ahead of use, so a restart never reuses one.
const SEQ_RESERVATION: u64 = 4_096;
/// How long the writer waits for a database connection while it holds the writer lock; the
/// pool's own default (30 s) would stall live publication and ingest for that long.
const CONNECTION_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
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

/// What a checkpoint records: ownership from the live map when the batch was committed, and
/// the official-API state recorded for each of those territories by then.
struct Capture {
    through_seq: i64,
    holdings: Vec<(String, GuildKey, i64)>,
    states: Vec<(String, i64, TerritoryState)>,
}

/// Official-API state changes at one stream position.
pub struct StateChanges {
    pub seq: u64,
    /// When the poller received the payload.
    pub observed_at: DateTime<Utc>,
    pub changes: Vec<(String, Vec<TerritoryStateField>)>,
}

struct PendingBatch {
    recorded_at: DateTime<Utc>,
    facts: Vec<PendingFact>,
    reports: Vec<Report>,
    states: Option<StateChanges>,
    checkpoint: Option<Capture>,
}

impl PendingBatch {
    fn items(&self) -> usize {
        let held = self
            .checkpoint
            .as_ref()
            .map_or(0, |capture| capture.holdings.len());
        let states = self
            .states
            .as_ref()
            .map_or(0, |states| states.changes.len());
        (self.facts.len() + self.reports.len() + states + held).max(1)
    }
}

/// One accepted change of the live map, ready to be made durable.
pub struct Commit<'a> {
    pub source: FactSource,
    /// Ownership changes with their stream position and order within it.
    pub changes: Vec<(u64, i16, &'a TerritoryChange)>,
    pub reports: Vec<Report>,
    /// Official-API state changes, from [`WriterState::state_changes`].
    pub states: Option<StateChanges>,
    /// The live map after this change, for a checkpoint when one is due.
    pub live: &'a TerritoryMap,
    /// Ownership state changed without a fact (a territory removed, a guild recoloured):
    /// only a checkpoint records it.
    pub needs_checkpoint: bool,
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
    /// Encoded state changes committed since the last checkpoint capture, stored or pending.
    state_bytes_since_capture: usize,
    last_capture_at: Option<DateTime<Utc>>,
    territories: HashMap<String, i16>,
    guilds: HashMap<GuildKey, i32>,
    /// The official-API state last staged for each territory of the latest poll, with when the
    /// API supplied its latest change; state changes are recorded against it.
    api_states: HashMap<String, (i64, TerritoryState)>,
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
        let checkpoint: Option<(DateTime<Utc>, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT taken_at, states FROM history_checkpoint \
             ORDER BY taken_at DESC, id DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        let since = checkpoint
            .as_ref()
            .map_or(super::beginning(), |(taken_at, _)| *taken_at);
        let (since_checkpoint, state_bytes): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM history_fact WHERE recorded_at > $1), \
                 (SELECT coalesce(sum(octet_length(changes)), 0)::int8 FROM history_api_state \
                  WHERE recorded_at > $1)",
        )
        .bind(since)
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

        let names: HashMap<i16, &str> = self
            .territories
            .iter()
            .map(|(name, id)| (*id, name.as_str()))
            .collect();
        let mut states: HashMap<i16, (i64, TerritoryState)> = match &checkpoint {
            Some((_, Some(bytes))) => checkpoint::decode_states(bytes)
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
                .into_iter()
                .map(|record| (record.territory_id, (record.observed_micros, record.state)))
                .collect(),
            _ => HashMap::new(),
        };
        let rows: Vec<(DateTime<Utc>, Vec<u8>)> = sqlx::query_as(
            "SELECT observed_at, changes FROM history_api_state WHERE recorded_at > $1 \
             ORDER BY stream_seq",
        )
        .bind(since)
        .fetch_all(pool)
        .await?;
        for (observed_at, bytes) in rows {
            let changes =
                state::decode(&bytes).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
            apply_state_changes(&mut states, micros(observed_at), changes);
        }
        self.api_states = states
            .into_iter()
            .filter_map(|(id, entry)| Some((names.get(&id)?.to_string(), entry)))
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
        self.state_bytes_since_capture = usize::try_from(state_bytes).unwrap_or(usize::MAX);
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
        let reserved = async {
            let mut connection = acquire(pool).await?;
            sqlx::query(
                "UPDATE history_meta \
                 SET seq_reserved_through = GREATEST(seq_reserved_through, $1)",
            )
            .bind(i64::try_from(through).unwrap_or(i64::MAX))
            .execute(&mut *connection)
            .await
        };
        match reserved.await {
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

    /// The official-API state of each territory in `current` that differs from what was last
    /// staged, as the changed fields. A territory without a staged state (seen for the first
    /// time, or after a dropped batch cleared them) gets every field, unknown ones included,
    /// so its row replaces whatever a reader held. Territories missing from `current` are
    /// forgotten, so one that returns is recorded in full.
    pub fn state_changes(
        &mut self,
        current: &HashMap<String, TerritoryState>,
    ) -> Vec<(String, Vec<TerritoryStateField>)> {
        self.api_states.retain(|name, _| current.contains_key(name));
        let mut changes: Vec<_> = current
            .iter()
            .filter_map(|(name, state)| {
                let fields = match self.api_states.get(name) {
                    Some((_, before)) => state.changes_from(before),
                    None => state.fields(),
                };
                (!fields.is_empty()).then(|| (name.clone(), fields))
            })
            .collect();
        changes.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        changes
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
        self.stage(commit);
        self.retry(pool).await
    }

    /// Queue a change for the next [`retry`](Self::retry), capturing a checkpoint of the live
    /// map now if one is due, so the caller can release the live map before any database
    /// work.
    pub fn stage(&mut self, commit: Commit<'_>) {
        if commit.changes.is_empty()
            && commit.reports.is_empty()
            && commit.states.is_none()
            && !commit.needs_checkpoint
        {
            return;
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
        if let Some(states) = &commit.states {
            for (name, fields) in &states.changes {
                let entry = self
                    .api_states
                    .entry(name.clone())
                    .or_insert_with(|| (0, TerritoryState::default()));
                entry.0 = micros(states.observed_at);
                for field in fields {
                    entry.1.apply(field.clone());
                }
            }
        }
        let state_seq = commit
            .states
            .as_ref()
            .map(|states| i64::try_from(states.seq).unwrap_or(i64::MAX));
        let added = u32::try_from(facts.len()).unwrap_or(u32::MAX);
        let state_bytes = commit
            .states
            .as_ref()
            .map_or(0, |states| state::encoded_len(&states.changes));
        let checkpoint_due = commit.needs_checkpoint
            || self.last_capture_at.is_none()
            || self.facts_since_capture.saturating_add(added) >= CHECKPOINT_FACT_BUDGET
            || self.state_bytes_since_capture.saturating_add(state_bytes) >= CHECKPOINT_STATE_BYTES
            || ((added > 0 || state_bytes > 0)
                && self.last_capture_at.is_some_and(|at| {
                    recorded_at - at >= Duration::seconds(CHECKPOINT_MAX_AGE_SECS)
                }));
        let checkpoint = checkpoint_due.then(|| Capture {
            through_seq: facts
                .iter()
                .map(|fact| fact.seq)
                .chain(state_seq)
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
            states: commit
                .live
                .keys()
                .filter_map(|name| {
                    let (observed, state) = self.api_states.get(name)?;
                    Some((name.clone(), *observed, state.clone()))
                })
                .collect(),
        });
        if checkpoint_due {
            self.facts_since_capture = 0;
            self.state_bytes_since_capture = 0;
            self.last_capture_at = Some(recorded_at);
        } else {
            self.facts_since_capture = self.facts_since_capture.saturating_add(added);
            self.state_bytes_since_capture =
                self.state_bytes_since_capture.saturating_add(state_bytes);
        }
        let batch = PendingBatch {
            recorded_at,
            facts,
            reports: commit.reports,
            states: commit.states,
            checkpoint,
        };
        self.pending_facts += batch.facts.len();
        self.pending_reports += batch.reports.len();
        self.pending_items += batch.items();
        self.pending.push_back(batch);
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
                    if oldest.states.is_some() {
                        // The next poll records every territory's state in full again.
                        self.api_states.clear();
                    }
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
            for (name, _) in batch.states.iter().flat_map(|states| &states.changes) {
                territory_names.insert(name);
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

        let mut connection = acquire(pool).await?;
        let mut tx = Connection::begin(&mut *connection).await?;
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
        archive::insert(&mut tx, &reports, archive::OnSameKey::Keep).await?;

        let mut state_rows = StateColumns::default();
        for batch in &self.pending {
            let Some(states) = &batch.states else {
                continue;
            };
            let changes = states
                .changes
                .iter()
                .map(|(name, fields)| Some((territory_id(name)?, fields.clone())))
                .collect::<Option<state::Changes>>()
                .ok_or_else(|| {
                    sqlx::Error::Protocol("history dictionary is missing a territory".into())
                })?;
            state_rows
                .seq
                .push(i64::try_from(states.seq).unwrap_or(i64::MAX));
            state_rows.recorded_at.push(batch.recorded_at);
            state_rows.observed_at.push(states.observed_at);
            state_rows.changes.push(state::encode(&changes));
        }
        if !state_rows.seq.is_empty() {
            state_rows.insert(&mut tx).await?;
        }

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
            let mut states: Vec<StateRecord> = capture
                .states
                .iter()
                .filter_map(|(name, observed_micros, state)| {
                    Some(StateRecord {
                        territory_id: territory_id(name)?,
                        observed_micros: *observed_micros,
                        state: state.clone(),
                    })
                })
                .collect();
            states.sort_unstable_by_key(|record| record.territory_id);
            sqlx::query(
                "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership, states) \
                 VALUES ($1, $2, 2, $3, $4)",
            )
            .bind(batch.recorded_at)
            .bind(capture.through_seq)
            .bind(checkpoint::encode(&holdings))
            .bind(checkpoint::encode_states(&states))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        self.territories.extend(new_territories);
        self.guilds.extend(new_guilds);
        Ok(())
    }
}

/// A pooled connection within [`CONNECTION_WAIT`]. Only the wait is bounded: nothing has
/// been written when it gives up, so the pending work is retried unchanged.
async fn acquire(
    pool: &sqlx::PgPool,
) -> Result<sqlx_core::pool::PoolConnection<sqlx::Postgres>, sqlx::Error> {
    tokio::time::timeout(CONNECTION_WAIT, pool.acquire())
        .await
        .map_err(|_| sqlx::Error::PoolTimedOut)?
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

/// Apply one row's changes to states keyed by territory id.
pub(crate) fn apply_state_changes(
    states: &mut HashMap<i16, (i64, TerritoryState)>,
    observed_micros: i64,
    changes: state::Changes,
) {
    for (territory_id, fields) in changes {
        let entry = states
            .entry(territory_id)
            .or_insert_with(|| (observed_micros, TerritoryState::default()));
        entry.0 = observed_micros;
        for field in fields {
            entry.1.apply(field);
        }
    }
}

#[derive(Default)]
struct StateColumns {
    seq: Vec<i64>,
    recorded_at: Vec<DateTime<Utc>>,
    observed_at: Vec<DateTime<Utc>>,
    changes: Vec<Vec<u8>>,
}

impl StateColumns {
    /// Insert rows and extend the history's time and stream bounds over them, so a state
    /// change after the last ownership fact is still within reach. Ownership counts are left
    /// alone. A retried batch whose earlier commit did land finds its rows already there.
    async fn insert(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "WITH inserted AS ( \
                 INSERT INTO history_api_state (stream_seq, recorded_at, observed_at, changes) \
                 SELECT * FROM UNNEST($1::int8[], $2::timestamptz[], $3::timestamptz[], \
                     $4::bytea[]) \
                 ON CONFLICT DO NOTHING \
                 RETURNING stream_seq, recorded_at \
             ) \
             UPDATE history_meta SET \
                 earliest_at = LEAST(earliest_at, (SELECT min(recorded_at) FROM inserted)), \
                 latest_at = GREATEST(latest_at, (SELECT max(recorded_at) FROM inserted)), \
                 latest_fact_seq = GREATEST(latest_fact_seq, \
                     (SELECT max(stream_seq) FROM inserted))",
        )
        .bind(self.seq)
        .bind(self.recorded_at)
        .bind(self.observed_at)
        .bind(self.changes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
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
        let commit = |changes, live, needs_checkpoint| Commit {
            source: FactSource::Poller,
            changes,
            reports: Vec::new(),
            live,
            states: None,
            needs_checkpoint,
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
                let mut names: Vec<String> = super::super::read::territories_at(&pool, at)
                    .await
                    .expect("seek")
                    .ownership
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

    struct Holding {
        guild: GuildRef,
        acquired: DateTime<Utc>,
    }

    impl Holding {
        fn new(guild: &str, acquired: &str) -> Self {
            Self {
                guild: GuildRef {
                    uuid: format!("{guild}-uuid"),
                    name: guild.to_string(),
                    prefix: guild.to_string(),
                    color: None,
                },
                acquired: acquired.parse().expect("acquired"),
            }
        }

        fn live(&self, name: &str) -> TerritoryMap {
            let territory = sequoia_shared::Territory {
                guild: self.guild.clone(),
                acquired: self.acquired,
                location: sequoia_shared::Region {
                    start: [0, 0],
                    end: [10, 10],
                },
                resources: sequoia_shared::Resources::default(),
                connections: Vec::new(),
                runtime: None,
            };
            TerritoryMap::from([(name.to_string(), territory)])
        }

        fn change(&self, name: &str, previous: Option<&Holding>) -> TerritoryChange {
            TerritoryChange {
                territory: name.to_string(),
                guild: self.guild.clone(),
                previous_guild: previous.map(|holding| holding.guild.clone()),
                acquired: self.acquired.to_rfc3339(),
                location: sequoia_shared::Region {
                    start: [0, 0],
                    end: [10, 10],
                },
                resources: sequoia_shared::Resources::default(),
                connections: Vec::new(),
                runtime: None,
            }
        }

        /// What the official API supplies for this holding.
        fn api_state(&self, headquarters: bool, stored: i32) -> TerritoryState {
            TerritoryState {
                owner: Some(sequoia_shared::StateOwner {
                    guild_uuid: self.guild.uuid.clone(),
                    acquired_micros: self.acquired.timestamp_micros(),
                }),
                headquarters: Some(headquarters),
                treasury: Some("LOW".to_string()),
                stored: Some(sequoia_shared::Resources {
                    emeralds: stored,
                    ..sequoia_shared::Resources::default()
                }),
                base_generation: Some(sequoia_shared::Resources {
                    emeralds: 9000,
                    ..sequoia_shared::Resources::default()
                }),
                links: Some(vec!["Llevigar Stables".to_string()]),
                ..TerritoryState::default()
            }
        }
    }

    /// One poll: ownership facts and the official state diff, as the poller commits them.
    async fn poll(
        writer: &mut WriterState,
        pool: &sqlx::PgPool,
        live: &TerritoryMap,
        facts: &[(u64, &TerritoryChange)],
        api: TerritoryState,
        state_seq: u64,
    ) {
        let current = HashMap::from([("Orc Road".to_string(), api)]);
        let changes = writer.state_changes(&current);
        let states = (!changes.is_empty()).then(|| StateChanges {
            seq: state_seq,
            observed_at: Utc::now(),
            changes,
        });
        let outcome = writer
            .commit(
                Some(pool),
                Commit {
                    source: FactSource::Poller,
                    changes: facts
                        .iter()
                        .map(|(seq, change)| (*seq, 0, *change))
                        .collect(),
                    reports: Vec::new(),
                    states,
                    live,
                    needs_checkpoint: false,
                },
            )
            .await;
        assert!(
            matches!(outcome, CommitOutcome::Durable { .. }),
            "{outcome:?}"
        );
        tokio::time::sleep(StdDuration::from_millis(3)).await;
    }

    #[tokio::test]
    async fn official_state_shows_only_for_the_holding_the_api_reported_it_for() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping official state test: TEST_DATABASE_URL is not set");
            return;
        };
        let state_at = |at| {
            let pool = pool.clone();
            async move {
                let territories = super::super::read::territories_at(&pool, at)
                    .await
                    .expect("seek");
                let owner = &territories.ownership["Orc Road"];
                let acquired = DateTime::parse_from_rfc3339(&owner.acquired_at)
                    .expect("acquired")
                    .timestamp_micros();
                let state = territories.states.get("Orc Road").map(|s| s.state.clone());
                // What the map may show for the holding: none of a state reported for another.
                let shown = state
                    .clone()
                    .filter(|state| state.reported_for(&owner.guild_uuid, acquired));
                (owner.guild_name.clone(), state, shown)
            }
        };
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");
        let avo = Holding::new("AVO", "2026-11-01T10:00:00.663Z");
        let first = avo.change("Orc Road", None);
        poll(
            &mut writer,
            &pool,
            &avo.live("Orc Road"),
            &[(1, &first)],
            avo.api_state(false, 40),
            2,
        )
        .await;

        // The headquarters moves here with no change of owner.
        poll(
            &mut writer,
            &pool,
            &avo.live("Orc Road"),
            &[],
            avo.api_state(true, 40),
            3,
        )
        .await;
        let (owner, _, shown) = state_at(Utc::now()).await;
        assert_eq!(
            (owner.as_str(), shown.and_then(|s| s.headquarters)),
            ("AVO", Some(true))
        );

        // A reporter's capture leads the API, with its own estimate of the acquisition time.
        let reported = Holding::new("Mood", "2026-11-02T11:17:33Z");
        let capture = reported.change("Orc Road", Some(&avo));
        let outcome = writer
            .commit(
                Some(&pool),
                Commit {
                    source: FactSource::Canonical,
                    changes: vec![(4, 0, &capture)],
                    reports: Vec::new(),
                    states: None,
                    live: &reported.live("Orc Road"),
                    needs_checkpoint: false,
                },
            )
            .await;
        assert_eq!(outcome, CommitOutcome::Durable { facts: 1 });
        // The API still reports AVO's holding, and its stockpile moves on.
        poll(
            &mut writer,
            &pool,
            &reported.live("Orc Road"),
            &[],
            avo.api_state(true, 90),
            5,
        )
        .await;
        let (owner, state, shown) = state_at(Utc::now()).await;
        let state = state.expect("the observation stays known");
        assert_eq!(owner, "Mood");
        assert_eq!(
            shown, None,
            "AVO's headquarters, treasury and stockpile are not Mood's"
        );
        assert_eq!(
            (state.headquarters, state.stored.map(|s| s.emeralds)),
            (Some(true), Some(90)),
            "it is kept as observed, for AVO's holding"
        );

        // The API reports the capture with its own time; the reporter's holding still differs.
        let official = Holding::new("Mood", "2026-11-02T11:17:34.195Z");
        poll(
            &mut writer,
            &pool,
            &reported.live("Orc Road"),
            &[],
            official.api_state(false, 0),
            6,
        )
        .await;
        let (_, state, shown) = state_at(Utc::now()).await;
        assert_eq!(shown, None);
        assert_eq!(state.and_then(|s| s.headquarters), Some(false));

        // Once the override lapses, the poller records the API's holding and its state shows.
        let settled = official.change("Orc Road", Some(&reported));
        poll(
            &mut writer,
            &pool,
            &official.live("Orc Road"),
            &[(7, &settled)],
            official.api_state(false, 0),
            8,
        )
        .await;
        let (_, _, shown) = state_at(Utc::now()).await;
        let state = shown.expect("state");
        assert_eq!(
            (state.headquarters, state.stored.map(|s| s.emeralds)),
            (Some(false), Some(0))
        );

        // A restart records nothing new for an unchanged API state.
        let mut restarted = WriterState::default();
        restarted.recover(&pool).await.expect("recover");
        let current = HashMap::from([("Orc Road".to_string(), official.api_state(false, 0))]);
        assert!(restarted.state_changes(&current).is_empty());

        // Pages hold facts and state rows in stream order and never split them.
        let mut positions = Vec::new();
        let mut after = (i64::MIN, i16::MIN);
        loop {
            let page = super::super::read::fact_page(
                &pool,
                super::super::beginning(),
                Utc::now(),
                after,
                2,
            )
            .await
            .expect("page");
            let mut seen: Vec<(i64, &str)> = page
                .facts
                .iter()
                .map(|fact| (fact.stream_seq, "fact"))
                .chain(
                    page.states
                        .iter()
                        .map(|state| (state.stream_seq as i64, "state")),
                )
                .collect();
            seen.sort_unstable();
            after = (seen.last().expect("page has rows").0, i16::MAX);
            positions.extend(seen);
            if !page.has_more {
                break;
            }
        }
        assert_eq!(
            positions,
            vec![
                (1, "fact"),
                (2, "state"),
                (3, "state"),
                (4, "fact"),
                (5, "state"),
                (6, "state"),
                (7, "fact"),
            ]
        );
    }

    #[tokio::test]
    async fn a_headquarters_move_after_the_last_owner_change_is_within_the_bounds() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping state bounds test: TEST_DATABASE_URL is not set");
            return;
        };
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");
        let avo = Holding::new("AVO", "2026-11-01T10:00:00Z");
        let live = avo.live("Orc Road");
        let first = avo.change("Orc Road", None);
        poll(
            &mut writer,
            &pool,
            &live,
            &[(1, &first)],
            avo.api_state(false, 0),
            2,
        )
        .await;
        poll(&mut writer, &pool, &live, &[], avo.api_state(true, 0), 3).await;

        let bounds = super::super::read::bounds(&pool).await.expect("bounds");
        assert_eq!(
            bounds.latest_seq,
            Some(3),
            "playback reaches the state-only position"
        );
        assert_eq!(bounds.fact_count, 1, "only the owner change is an event");
        let latest = bounds.latest.expect("latest");
        let seen = super::super::read::territories_at(&pool, latest)
            .await
            .expect("seek");
        assert_eq!(
            seen.states["Orc Road"].state.headquarters,
            Some(true),
            "a seek to the end shows the move"
        );
        let page = super::super::read::fact_page(
            &pool,
            super::super::beginning(),
            latest,
            (1, i16::MAX),
            10,
        )
        .await
        .expect("page");
        assert_eq!(
            page.states.iter().map(|s| s.stream_seq).collect::<Vec<_>>(),
            vec![2, 3]
        );

        let mut restarted = WriterState::default();
        assert_eq!(restarted.recover(&pool).await.expect("recover"), 3);

        // After a rollback the previous server writes checkpoints without states; once this
        // one is back, the next poll records the full state rather than building on them.
        sqlx::query(
            "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership) \
             SELECT now(), through_seq, 2, ownership FROM history_checkpoint \
             ORDER BY id DESC LIMIT 1",
        )
        .execute(&pool)
        .await
        .expect("checkpoint without states");
        let mut rolled_forward = WriterState::default();
        rolled_forward.recover(&pool).await.expect("recover");
        let current = HashMap::from([("Orc Road".to_string(), avo.api_state(true, 0))]);
        let changes = rolled_forward.state_changes(&current);
        assert_eq!(changes.len(), 1);
        assert!(
            changes[0]
                .1
                .contains(&TerritoryStateField::Headquarters(Some(true)))
        );
    }

    #[tokio::test]
    async fn a_clear_lost_in_an_outage_is_repaired_by_a_full_row() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping state repair test: TEST_DATABASE_URL is not set");
            return;
        };
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");
        let avo = Holding::new("AVO", "2026-11-01T10:00:00Z");
        let live = avo.live("Orc Road");
        let first = avo.change("Orc Road", None);
        let high = TerritoryState {
            treasury: Some("HIGH".to_string()),
            ..avo.api_state(true, 10)
        };
        let cleared = TerritoryState {
            treasury: None,
            ..high.clone()
        };
        poll(&mut writer, &pool, &live, &[(1, &first)], high, 2).await;
        let checkpoints = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM history_checkpoint")
                .fetch_one(&pool)
                .await
                .expect("count")
        };
        assert_eq!(checkpoints().await, 1, "the checkpoint holds treasury HIGH");

        // The API stops supplying the treasury while the database is down, and the queue
        // overflows before it returns, dropping that change.
        let outage = unreachable_pool();
        let current = HashMap::from([("Orc Road".to_string(), cleared.clone())]);
        let changes = writer.state_changes(&current);
        assert_eq!(changes[0].1, vec![TerritoryStateField::Treasury(None)]);
        let outcome = writer
            .commit(
                Some(&outage),
                Commit {
                    source: FactSource::Poller,
                    changes: Vec::new(),
                    reports: Vec::new(),
                    states: Some(StateChanges {
                        seq: 3,
                        observed_at: Utc::now(),
                        changes,
                    }),
                    live: &live,
                    needs_checkpoint: false,
                },
            )
            .await;
        assert!(matches!(outcome, CommitOutcome::Deferred { .. }));
        let mut dropped = 0;
        while dropped == 0 {
            let outcome = writer
                .commit(
                    Some(&outage),
                    Commit {
                        source: FactSource::Canonical,
                        changes: Vec::new(),
                        reports: runtime_reports(5_000),
                        states: None,
                        live: &live,
                        needs_checkpoint: false,
                    },
                )
                .await;
            let CommitOutcome::Deferred {
                dropped_reports, ..
            } = outcome
            else {
                panic!("deferred, got {outcome:?}");
            };
            dropped += dropped_reports;
        }

        // Back online, the next poll replaces the state in full, unknown treasury included.
        poll(&mut writer, &pool, &live, &[], cleared.clone(), 4).await;
        assert_eq!(checkpoints().await, 1, "no checkpoint hides the repair row");
        let seen = super::super::read::territories_at(&pool, Utc::now())
            .await
            .expect("seek");
        assert_eq!(seen.states["Orc Road"].state, cleared);
        let page = super::super::read::fact_page(
            &pool,
            super::super::beginning(),
            Utc::now(),
            (2, i16::MAX),
            10,
        )
        .await
        .expect("page");
        let repair = &page.states.last().expect("repair row").changes[0].fields;
        assert!(repair.contains(&TerritoryStateField::Treasury(None)));
        assert_eq!(repair.len(), cleared.fields().len());

        let mut restarted = WriterState::default();
        restarted.recover(&pool).await.expect("recover");
        assert!(restarted.state_changes(&current).is_empty());
    }

    #[tokio::test]
    async fn retention_keeps_the_earliest_bound_over_both_kinds_of_history() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping retention bounds test: TEST_DATABASE_URL is not set");
            return;
        };
        let at = |minute: i64| {
            "2026-01-01T00:00:00Z"
                .parse::<DateTime<Utc>>()
                .expect("base")
                + Duration::minutes(minute)
        };
        // A fact, state row or checkpoint at each minute, with stream positions in time order.
        let history = |facts: &'static [i64],
                       states: &'static [i64],
                       checkpoints: &'static [i64]| {
            let pool = pool.clone();
            async move {
                crate::test_support::reset_history(&pool).await;
                for &minute in facts {
                    sqlx::query(
                        "INSERT INTO history_fact (stream_seq, recorded_at, acquired_at, guild_id, \
                             ordinal, territory_id, source, take) \
                         VALUES ($1, $2, $2, 1, 0, 1, 1, false)",
                    )
                    .bind(minute)
                    .bind(at(minute))
                    .execute(&pool)
                    .await
                    .expect("fact");
                }
                for &minute in states {
                    sqlx::query(
                        "INSERT INTO history_api_state (stream_seq, recorded_at, observed_at, changes) \
                         VALUES ($1, $2, $2, '\\x0100')",
                    )
                    .bind(minute)
                    .bind(at(minute))
                    .execute(&pool)
                    .await
                    .expect("state row");
                }
                for &minute in checkpoints {
                    sqlx::query(
                        "INSERT INTO history_checkpoint (taken_at, through_seq, source, ownership) \
                         VALUES ($1, $2, 2, '\\x01')",
                    )
                    .bind(at(minute))
                    .bind(minute)
                    .execute(&pool)
                    .await
                    .expect("checkpoint");
                }
                let earliest = [facts, states].concat().into_iter().min().map(at);
                sqlx::query("UPDATE history_meta SET earliest_at = $1, fact_count = $2")
                    .bind(earliest)
                    .bind(facts.len() as i64)
                    .execute(&pool)
                    .await
                    .expect("meta");
                super::super::read::expire_before(&pool, at(15))
                    .await
                    .expect("expire");
                sqlx::query_as::<_, (Option<DateTime<Utc>>, i64)>(
                    "SELECT earliest_at, fact_count FROM history_meta",
                )
                .fetch_one(&pool)
                .await
                .expect("meta")
            }
        };

        // State changes retained before the next owner change stay seekable.
        assert_eq!(history(&[5, 30], &[20, 25], &[10]).await, (Some(at(20)), 1));
        // Retained state changes with no ownership fact left still bound the history.
        assert_eq!(history(&[5], &[20, 25], &[10]).await, (Some(at(20)), 0));
        // Removing only state rows moves the bound past them; the fact count is unchanged.
        assert_eq!(history(&[30], &[5], &[10]).await, (Some(at(30)), 1));
    }

    #[tokio::test]
    async fn rich_state_churn_brings_checkpoints_that_bound_seek_replay() {
        let Some((pool, _lock)) = reset_test_database().await else {
            eprintln!("Skipping state checkpoint test: TEST_DATABASE_URL is not set");
            return;
        };
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");
        let avo = Holding::new("AVO", "2026-11-01T10:00:00Z");
        let live = avo.live("Orc Road");
        let first = avo.change("Orc Road", None);
        let mut api = avo.api_state(false, 0);
        poll(&mut writer, &pool, &live, &[(1, &first)], api.clone(), 2).await;
        let checkpoints = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM history_checkpoint")
                .fetch_one(&pool)
                .await
                .expect("count")
        };
        let initial = checkpoints().await;
        // Links long enough that a few rows pass the replay budget.
        for seq in 3..12u64 {
            api.links = Some(vec![format!("{seq}").repeat(CHECKPOINT_STATE_BYTES / 4)]);
            poll(&mut writer, &pool, &live, &[], api.clone(), seq).await;
        }
        assert!(
            checkpoints().await > initial,
            "state churn alone brings a checkpoint"
        );
        let (states, replay): (Option<Vec<u8>>, i64) = sqlx::query_as(
            "SELECT c.states, (SELECT coalesce(sum(octet_length(changes)), 0)::int8 \
                 FROM history_api_state WHERE recorded_at > c.taken_at) \
             FROM history_checkpoint c ORDER BY taken_at DESC, id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("latest checkpoint");
        let states = checkpoint::decode_states(&states.expect("the checkpoint carries states"))
            .expect("states decode");
        assert_eq!(states.len(), 1);
        assert!(
            replay < CHECKPOINT_STATE_BYTES as i64,
            "a seek decodes {replay} bytes"
        );
        let seen = super::super::read::territories_at(&pool, Utc::now())
            .await
            .expect("seek");
        assert_eq!(seen.states["Orc Road"].state.links, api.links);
    }

    #[tokio::test]
    async fn an_occupied_pool_defers_promptly_and_the_work_commits_once_it_frees() {
        let Some((setup, _lock)) = reset_test_database().await else {
            eprintln!("Skipping occupied pool test: TEST_DATABASE_URL is not set");
            return;
        };
        let url = std::env::var("TEST_DATABASE_URL").expect("database url");
        // The pool's own acquire timeout stays at its 30 s default; only the writer bounds it.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect server pool");
        let mut writer = WriterState::default();
        writer.recover(&pool).await.expect("recover");
        let held = pool.acquire().await.expect("occupy the only connection");

        let territory: sequoia_shared::Territory = serde_json::from_value(serde_json::json!({
            "guild": {"uuid": "g", "name": "Guild", "prefix": "G"},
            "acquired": "2026-01-01T00:00:00Z",
            "location": {"start": [0, 0], "end": [1, 1]}
        }))
        .expect("territory");
        let change = TerritoryChange {
            territory: "Alpha".to_string(),
            guild: territory.guild.clone(),
            previous_guild: None,
            acquired: territory.acquired.to_rfc3339(),
            location: territory.location.clone(),
            resources: territory.resources.clone(),
            connections: Vec::new(),
            runtime: None,
        };
        let live = TerritoryMap::from([("Alpha".to_string(), territory)]);
        let started = std::time::Instant::now();
        let outcome = writer
            .commit(
                Some(&pool),
                Commit {
                    source: FactSource::Poller,
                    changes: vec![(1, 0, &change)],
                    reports: Vec::new(),
                    live: &live,
                    states: None,
                    needs_checkpoint: false,
                },
            )
            .await;
        assert!(
            started.elapsed() < StdDuration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(
            outcome,
            CommitOutcome::Deferred {
                pending_facts: 1,
                dropped_facts: 0,
                ..
            }
        ));
        assert!(writer.has_pending());

        drop(held);
        assert_eq!(
            writer.retry(&pool).await,
            CommitOutcome::Durable { facts: 1 }
        );
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM history_fact")
            .fetch_one(&setup)
            .await
            .expect("count facts");
        assert_eq!(stored, 1);
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
                        states: None,
                        needs_checkpoint: false,
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
                    states: None,
                    needs_checkpoint: true,
                },
            )
            .await;
        assert!(matches!(outcome, CommitOutcome::Deferred { .. }));
        assert!(writer.pending_items <= MAX_PENDING_ITEMS);
        assert!(writer.pending_items > before.saturating_sub(per_batch));
    }
}
