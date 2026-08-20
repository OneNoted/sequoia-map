use std::time::Duration;

use tracing::{info, warn};

use crate::config::{
    RETENTION_CHECK_SECS, season_history_retention_days, territory_history_retention_days,
};
use crate::state::AppState;

const BATCH_SIZE: i64 = 10_000;

/// Daily cleanup of old history data beyond the retention period.
pub async fn run(state: AppState) {
    let Some(pool) = state.db.as_ref().cloned() else {
        warn!("retention cleaner disabled: no database configured");
        return;
    };
    let territory_retention_days = territory_history_retention_days();
    let season_retention_days = season_history_retention_days();

    info!(
        "Retention cleaner started (territory retention: {}d, season retention: {}d, check interval: {}s)",
        territory_retention_days, season_retention_days, RETENTION_CHECK_SECS
    );

    run_cleanup_once(&pool, territory_retention_days, season_retention_days).await;

    let mut interval = tokio::time::interval(Duration::from_secs(RETENTION_CHECK_SECS));
    // Consume immediate tick so subsequent cleanup runs after the configured interval.
    interval.tick().await;

    loop {
        interval.tick().await;
        run_cleanup_once(&pool, territory_retention_days, season_retention_days).await;
    }
}

async fn run_cleanup_once(
    pool: &sqlx::PgPool,
    territory_retention_days: i64,
    season_retention_days: i64,
) {
    let territory_cutoff = chrono::Utc::now() - chrono::Duration::days(territory_retention_days);
    let season_cutoff = chrono::Utc::now() - chrono::Duration::days(season_retention_days);

    // Delete old events in batches to avoid long locks
    let mut total_events = 0i64;
    loop {
        match sqlx::query(
            "DELETE FROM territory_events WHERE id IN \
             (SELECT id FROM territory_events WHERE recorded_at < $1 LIMIT $2)",
        )
        .bind(territory_cutoff)
        .bind(BATCH_SIZE)
        .execute(pool)
        .await
        {
            Ok(result) => {
                let deleted = result.rows_affected() as i64;
                total_events += deleted;
                if deleted < BATCH_SIZE {
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to delete old events: {e}");
                break;
            }
        }
    }

    // Delete old snapshots in batches
    let mut total_snapshots = 0i64;
    loop {
        match sqlx::query(
            "DELETE FROM territory_snapshots WHERE id IN \
             (SELECT id FROM territory_snapshots WHERE created_at < $1 LIMIT $2)",
        )
        .bind(territory_cutoff)
        .bind(BATCH_SIZE)
        .execute(pool)
        .await
        {
            Ok(result) => {
                let deleted = result.rows_affected() as i64;
                total_snapshots += deleted;
                if deleted < BATCH_SIZE {
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to delete old snapshots: {e}");
                break;
            }
        }
    }

    let total_scalar_samples =
        match sqlx::query("DELETE FROM season_scalar_samples WHERE sampled_at < $1")
            .bind(season_cutoff)
            .execute(pool)
            .await
        {
            Ok(result) => result.rows_affected() as i64,
            Err(e) => {
                warn!("Failed to delete old season scalar samples: {e}");
                0
            }
        };

    let total_season_observations =
        match sqlx::query("DELETE FROM season_guild_observations WHERE observed_at < $1")
            .bind(season_cutoff)
            .execute(pool)
            .await
        {
            Ok(result) => result.rows_affected() as i64,
            Err(e) => {
                warn!("Failed to delete old season guild observations: {e}");
                0
            }
        };

    if total_events > 0
        || total_snapshots > 0
        || total_scalar_samples > 0
        || total_season_observations > 0
    {
        info!(
            "Retention cleanup: removed {total_events} events and {total_snapshots} snapshots older than {territory_retention_days}d; removed {total_scalar_samples} scalar samples and {total_season_observations} season observations older than {season_retention_days}d"
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sqlx::postgres::PgPoolOptions;

    use super::run_cleanup_once;

    const REAL_DB_TEST_LOCK: i64 = 73_019_001;

    #[tokio::test]
    async fn run_cleanup_once_removes_old_scalar_samples() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping retention cleaner scalar test: DATABASE_URL is not set");
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
            .expect("acquire retention test lock");
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
        let old = now - chrono::Duration::days(366);
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(old)
        .bind(28_i32)
        .bind(1.5_f64)
        .bind(1.8_f64)
        .bind(0.50_f64)
        .bind(3_i32)
        .execute(&pool)
        .await
        .expect("insert old sample");
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(now)
        .bind(29_i32)
        .bind(2.2_f64)
        .bind(2.4_f64)
        .bind(0.75_f64)
        .bind(6_i32)
        .execute(&pool)
        .await
        .expect("insert current sample");

        sqlx::query(
            "INSERT INTO season_guild_observations \
             (observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(old)
        .bind(28_i32)
        .bind("OldGuild")
        .bind("uuid-old")
        .bind("OLD")
        .bind(4_i16)
        .bind(1120_i32)
        .execute(&pool)
        .await
        .expect("insert old season observation");
        sqlx::query(
            "INSERT INTO season_guild_observations \
             (observed_at, season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(now)
        .bind(29_i32)
        .bind("CurrentGuild")
        .bind("uuid-current")
        .bind("CUR")
        .bind(6_i16)
        .bind(1890_i32)
        .execute(&pool)
        .await
        .expect("insert current season observation");

        run_cleanup_once(&pool, 365, 365).await;

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM season_scalar_samples")
            .fetch_one(&pool)
            .await
            .expect("count scalar rows");
        assert_eq!(count.0, 1);

        let season: (i32,) = sqlx::query_as("SELECT season_id FROM season_scalar_samples LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("select remaining season");
        assert_eq!(season.0, 29);

        let obs_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM season_guild_observations")
            .fetch_one(&pool)
            .await
            .expect("count remaining season observations");
        assert_eq!(obs_count.0, 1);

        let obs_season: (i32,) =
            sqlx::query_as("SELECT season_id FROM season_guild_observations LIMIT 1")
                .fetch_one(&pool)
                .await
                .expect("select remaining season observation");
        assert_eq!(obs_season.0, 29);

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("release retention test lock");
    }
}
