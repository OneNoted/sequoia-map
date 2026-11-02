//! Helpers the server's unit tests share.

use chrono::{DateTime, Utc};

/// An RFC 3339 test timestamp.
pub(crate) fn ts(value: &str) -> DateTime<Utc> {
    value.parse::<DateTime<Utc>>().expect("parse timestamp")
}

/// Empty the legacy and compact history tables of a disposable test database.
pub(crate) async fn reset_history(pool: &sqlx::PgPool) {
    sqlx::query(
        "TRUNCATE TABLE territory_events, territory_snapshots, season_scalar_samples, \
         season_guild_observations, guild_color_cache, history_fact, history_checkpoint, \
         history_heat_day, history_guild, history_territory, history_api_state, canonical_report, \
         canonical_static, canonical_source, canonical_territory_updates RESTART IDENTITY",
    )
    .execute(pool)
    .await
    .expect("truncate history tables");
    sqlx::query("DELETE FROM history_meta")
        .execute(pool)
        .await
        .expect("clear history metadata");
    sqlx::query("INSERT INTO history_meta (id) VALUES (TRUE)")
        .execute(pool)
        .await
        .expect("reset history metadata");
}
