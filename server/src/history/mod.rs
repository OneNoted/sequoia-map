//! Territory history: one serialized writer for live ownership and official-API state changes,
//! compact storage (migrations 013 and 014), bounded reads and conversion from the legacy tables.

pub mod archive;
pub mod checkpoint;
pub mod convert;
pub mod read;
pub mod state;
pub mod writer;

use chrono::{DateTime, Utc};

/// Facts stored per checkpoint interval at most; seeks replay no more than this.
pub const CHECKPOINT_FACT_BUDGET: u32 = 4_000;
/// Encoded state changes stored per checkpoint interval at most; seeks decode no more than
/// this beyond the checkpoint.
pub const CHECKPOINT_STATE_BYTES: usize = 512 * 1024;
/// A checkpoint is also written once this much time has passed with any change.
pub const CHECKPOINT_MAX_AGE_SECS: i64 = 6 * 3600;

/// Origin of a recorded fact; converted legacy events are stored as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum FactSource {
    Poller = 1,
    Canonical = 2,
}

pub fn pack_color(color: Option<(u8, u8, u8)>) -> Option<i32> {
    color.map(|(r, g, b)| (i32::from(r) << 16) | (i32::from(g) << 8) | i32::from(b))
}

pub fn unpack_color(color: Option<i32>) -> Option<(u8, u8, u8)> {
    let color = u32::try_from(color?).ok()?;
    let [_, r, g, b] = color.to_be_bytes();
    Some((r, g, b))
}

pub fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

pub fn from_micros(micros: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(micros).unwrap_or(DateTime::UNIX_EPOCH)
}

/// The earliest and latest instants PostgreSQL stores, for open interval bounds.
pub fn beginning() -> DateTime<Utc> {
    DateTime::from_timestamp(-62_135_596_800, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

pub fn end_of_time() -> DateTime<Utc> {
    DateTime::from_timestamp(253_402_300_799, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

/// Whether a legacy table still exists; they may be retired after conversion.
pub async fn table_exists(pool: &sqlx::PgPool, table: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(pool)
        .await
}
