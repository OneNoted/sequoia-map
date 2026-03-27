use std::time::Duration;

use chrono::{DateTime, Utc};

pub const WYNNCRAFT_TERRITORY_URL: &str = "https://api.wynncraft.com/v3/guild/list/territory";
pub const WYNNCRAFT_GUILD_URL: &str = "https://api.wynncraft.com/v3/guild";
pub const WYNNCRAFT_GUILD_LIST_URL: &str = "https://api.wynncraft.com/v3/guild/list/guild";

pub const TERREXTRA_URL: &str = "https://gist.githubusercontent.com/Zatzou/14c82f2df0eb4093dfa1d543b78a73a8/raw/d03273fce33c031498c07e21b94f17644c8aae98/terrextra.json";
pub const TERREXTRA_REFRESH_SECS: u64 = 3600; // re-fetch hourly

pub const ATHENA_TERRITORY_URL: &str = "https://athena.wynntils.com/cache/get/territoryList";
pub const ATHENA_REFRESH_SECS: u64 = 600; // 10 minutes

pub const POLL_INTERVAL_SECS: u64 = 10;
pub const GUILD_CACHE_TTL_SECS: i64 = 600; // 10 minutes
pub const DEFAULT_GUILDS_ONLINE_CACHE_TTL_SECS: i64 = 120; // 2 minutes
pub const DEFAULT_GUILDS_ONLINE_MAX_CONCURRENCY: usize = 8;
pub const DEFAULT_SEASON_RATING_CONTENDER_COUNT: usize = 10;
pub const MAX_GUILD_CACHE_ENTRIES: usize = 64;
pub const SSE_KEEPALIVE_SECS: u64 = 15;
pub const DEFAULT_BROADCAST_BUFFER: usize = 256;
pub const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
pub const DEFAULT_UPSTREAM_HTTP_TIMEOUT_SECS: u64 = 10;
pub const DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECS: u64 = 3;
pub const DEFAULT_MAP_DOMAIN: &str = "map.example.com";
pub const SERVER_PORT: u16 = 3000;
pub const DEFAULT_CANONICAL_OVERRIDE_TTL_SECS: u64 = 180;
pub const DEFAULT_API_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_INGEST_UPDATES_PER_REQUEST: usize = 1024;
pub const DEFAULT_MAX_HISTORY_REPLAY_EVENTS: i64 = 20_000;
pub const DEFAULT_MAX_HISTORY_SR_SAMPLE_ROWS: i64 = 20_000;
pub const DEFAULT_SEASON_RACE_TOP_GUILDS: usize = 10;
pub const DEFAULT_SEASON_RACE_LOOKBACK_HOURS: i64 = 24;
pub const MIN_INTERNAL_INGEST_TOKEN_LEN: usize = 24;

// History feature
pub const SNAPSHOT_INTERVAL_SECS: u64 = 21600; // every 6 hours
pub const RETENTION_DAYS: i64 = 365;
pub const RETENTION_CHECK_SECS: u64 = 86400; // daily

const INTERNAL_INGEST_TOKEN_REJECTED_VALUES: &[&str] = &[
    "changeme",
    "change-me",
    "dev-internal-ingest-token",
    "default",
    "placeholder",
    "test-token",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveSeasonRaceConfig {
    pub season_id: i32,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub label: Option<String>,
    pub top_guilds: usize,
    pub lookback_hours: i64,
}

pub fn seq_live_handoff_v1_enabled() -> bool {
    std::env::var("SEQ_LIVE_HANDOFF_V1")
        .or_else(|_| std::env::var("seq_live_handoff_v1"))
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(true)
}

pub fn db_max_connections() -> u32 {
    std::env::var("DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DB_MAX_CONNECTIONS)
}

pub fn sse_broadcast_buffer() -> usize {
    std::env::var("SSE_BROADCAST_BUFFER")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_BROADCAST_BUFFER)
}

pub fn upstream_http_timeout() -> Duration {
    std::env::var("UPSTREAM_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_UPSTREAM_HTTP_TIMEOUT_SECS))
}

pub fn upstream_connect_timeout() -> Duration {
    std::env::var("UPSTREAM_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_UPSTREAM_CONNECT_TIMEOUT_SECS))
}

pub fn guilds_online_cache_ttl_secs() -> i64 {
    std::env::var("GUILDS_ONLINE_CACHE_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GUILDS_ONLINE_CACHE_TTL_SECS)
}

pub fn guilds_online_max_concurrency() -> usize {
    std::env::var("GUILDS_ONLINE_MAX_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GUILDS_ONLINE_MAX_CONCURRENCY)
}

pub fn season_rating_contender_count() -> usize {
    std::env::var("SEASON_RATING_CONTENDER_COUNT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SEASON_RATING_CONTENDER_COUNT)
}

pub fn season_rating_watchlist() -> Vec<String> {
    let Some(raw) = std::env::var("SEASON_RATING_WATCHLIST").ok() else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = normalize_watchlist_key(name);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        entries.push(name.to_string());
    }
    entries
}

pub fn internal_ingest_token() -> Option<String> {
    std::env::var("INTERNAL_INGEST_TOKEN")
        .or_else(|_| std::env::var("internal_ingest_token"))
        .ok()
        .and_then(|value| sanitize_internal_ingest_token(&value))
}

pub fn api_body_limit_bytes() -> usize {
    std::env::var("API_BODY_LIMIT_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_API_BODY_LIMIT_BYTES)
}

pub fn max_ingest_updates_per_request() -> usize {
    std::env::var("MAX_INGEST_UPDATES_PER_REQUEST")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_INGEST_UPDATES_PER_REQUEST)
}

pub fn max_history_replay_events() -> i64 {
    std::env::var("MAX_HISTORY_REPLAY_EVENTS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_HISTORY_REPLAY_EVENTS)
}

pub fn max_history_sr_sample_rows() -> i64 {
    std::env::var("MAX_HISTORY_SR_SAMPLE_ROWS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_HISTORY_SR_SAMPLE_ROWS)
}

pub fn season_race_top_guilds() -> usize {
    std::env::var("SEASON_RACE_TOP_GUILDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SEASON_RACE_TOP_GUILDS)
}

pub fn season_race_lookback_hours() -> i64 {
    std::env::var("SEASON_RACE_LOOKBACK_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SEASON_RACE_LOOKBACK_HOURS)
}

pub fn active_season_race_config() -> Result<Option<ActiveSeasonRaceConfig>, String> {
    parse_active_season_race_config(
        std::env::var("SEASON_RACE_ACTIVE_SEASON_ID")
            .ok()
            .as_deref(),
        std::env::var("SEASON_RACE_ACTIVE_START_AT").ok().as_deref(),
        std::env::var("SEASON_RACE_ACTIVE_END_AT").ok().as_deref(),
        std::env::var("SEASON_RACE_LABEL").ok().as_deref(),
        season_race_top_guilds(),
        season_race_lookback_hours(),
    )
}

fn parse_active_season_race_config(
    season_id: Option<&str>,
    start_at: Option<&str>,
    end_at: Option<&str>,
    label: Option<&str>,
    top_guilds: usize,
    lookback_hours: i64,
) -> Result<Option<ActiveSeasonRaceConfig>, String> {
    if season_id.is_none() && start_at.is_none() && end_at.is_none() {
        return Ok(None);
    }

    let season_id = season_id
        .ok_or_else(|| "SEASON_RACE_ACTIVE_SEASON_ID is required".to_string())?
        .trim()
        .parse::<i32>()
        .map_err(|_| "SEASON_RACE_ACTIVE_SEASON_ID must be a valid integer".to_string())?;
    let start_at = parse_rfc3339_utc(
        start_at.ok_or_else(|| "SEASON_RACE_ACTIVE_START_AT is required".to_string())?,
        "SEASON_RACE_ACTIVE_START_AT",
    )?;
    let end_at = parse_rfc3339_utc(
        end_at.ok_or_else(|| "SEASON_RACE_ACTIVE_END_AT is required".to_string())?,
        "SEASON_RACE_ACTIVE_END_AT",
    )?;
    if end_at <= start_at {
        return Err(
            "SEASON_RACE_ACTIVE_END_AT must be after SEASON_RACE_ACTIVE_START_AT".to_string(),
        );
    }

    let label = label
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    Ok(Some(ActiveSeasonRaceConfig {
        season_id,
        start_at,
        end_at,
        label,
        top_guilds,
        lookback_hours,
    }))
}

pub fn canonical_override_ttl() -> Duration {
    std::env::var("CANONICAL_OVERRIDE_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_CANONICAL_OVERRIDE_TTL_SECS))
}

pub fn map_public_base_url() -> String {
    std::env::var("MAP_DOMAIN")
        .ok()
        .as_deref()
        .map(normalize_public_base_url)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("https://{DEFAULT_MAP_DOMAIN}"))
}

fn normalize_public_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn parse_rfc3339_utc(raw: &str, env_name: &str) -> Result<DateTime<Utc>, String> {
    raw.trim()
        .parse::<DateTime<Utc>>()
        .map_err(|_| format!("{env_name} must be a valid RFC3339 timestamp"))
}

fn normalize_watchlist_key(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn sanitize_internal_ingest_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() < MIN_INTERNAL_INGEST_TOKEN_LEN {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    if INTERNAL_INGEST_TOKEN_REJECTED_VALUES.contains(&normalized.as_str()) {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveSeasonRaceConfig, DEFAULT_API_BODY_LIMIT_BYTES, normalize_public_base_url,
        normalize_watchlist_key, parse_active_season_race_config, sanitize_internal_ingest_token,
    };
    use chrono::{DateTime, Utc};

    #[test]
    fn sanitize_internal_ingest_token_rejects_short_or_placeholder_values() {
        assert_eq!(sanitize_internal_ingest_token(""), None);
        assert_eq!(sanitize_internal_ingest_token("test-token"), None);
        assert_eq!(
            sanitize_internal_ingest_token("  dev-internal-ingest-token  "),
            None
        );
        assert_eq!(sanitize_internal_ingest_token("0123456789"), None);
    }

    #[test]
    fn sanitize_internal_ingest_token_accepts_long_non_placeholder_values() {
        assert_eq!(
            sanitize_internal_ingest_token("  this-is-a-long-random-token-value-12345 "),
            Some("this-is-a-long-random-token-value-12345".to_string())
        );
    }

    #[test]
    fn default_api_body_limit_matches_ingest_forwarder_default_size() {
        assert_eq!(DEFAULT_API_BODY_LIMIT_BYTES, 2 * 1024 * 1024);
    }

    #[test]
    fn normalize_public_base_url_adds_https_and_trims_trailing_slashes() {
        assert_eq!(
            normalize_public_base_url(" map.seqwawa.com/ "),
            "https://map.seqwawa.com"
        );
        assert_eq!(
            normalize_public_base_url("https://seqwawa.com///"),
            "https://seqwawa.com"
        );
        assert_eq!(normalize_public_base_url(""), "");
    }

    #[test]
    fn normalize_watchlist_key_collapses_spacing_and_lowercases() {
        assert_eq!(
            normalize_watchlist_key("  Titans   Valor "),
            "titans valor".to_string()
        );
    }

    #[test]
    fn active_season_race_config_returns_none_when_unconfigured() {
        assert_eq!(
            parse_active_season_race_config(None, None, None, None, 10, 24).expect("config lookup"),
            None
        );
    }

    #[test]
    fn active_season_race_config_parses_current_season_settings() {
        let parsed = parse_active_season_race_config(
            Some("29"),
            Some("2026-03-01T00:00:00Z"),
            Some("2026-04-01T00:00:00Z"),
            Some("Season 29"),
            12,
            18,
        )
        .expect("parse config");
        assert_eq!(
            parsed,
            Some(ActiveSeasonRaceConfig {
                season_id: 29,
                start_at: "2026-03-01T00:00:00Z"
                    .parse::<DateTime<Utc>>()
                    .expect("parse start"),
                end_at: "2026-04-01T00:00:00Z"
                    .parse::<DateTime<Utc>>()
                    .expect("parse end"),
                label: Some("Season 29".to_string()),
                top_guilds: 12,
                lookback_hours: 18,
            })
        );
    }
}
