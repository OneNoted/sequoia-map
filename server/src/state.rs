use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sequoia_shared::{
    GuildRef, LiveState, Resources, SeasonScalarSample, TerritoryMap, TerritoryRuntimeData,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{RwLock, broadcast};
use tracing::warn;

use crate::config::{
    internal_ingest_token, max_history_replay_events, max_history_sr_sample_rows,
    max_ingest_updates_per_request, seq_live_handoff_v1_enabled, sse_broadcast_buffer,
    upstream_connect_timeout, upstream_http_timeout,
};

pub type GuildColor = (u8, u8, u8);
pub type GuildColorMap = HashMap<String, GuildColor>;
pub type CachedScalarSample = (SeasonScalarSample, Arc<Bytes>);

/// Normalize guild names for resilient color cache lookups.
/// - trims leading/trailing whitespace
/// - collapses interior whitespace to single spaces
/// - lowercases using Unicode rules
pub fn normalize_guild_color_key(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = String::with_capacity(trimmed.len());
    let mut saw_whitespace = false;

    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !saw_whitespace {
                normalized.push(' ');
                saw_whitespace = true;
            }
            continue;
        }

        saw_whitespace = false;
        for lower in ch.to_lowercase() {
            normalized.push(lower);
        }
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Build a normalized guild-color index for robust case/spacing-insensitive lookups.
pub fn build_guild_color_lookup(colors: &GuildColorMap) -> GuildColorMap {
    let mut lookup = HashMap::with_capacity(colors.len());
    for (guild_name, color) in colors {
        if let Some(key) = normalize_guild_color_key(guild_name) {
            lookup.entry(key).or_insert(*color);
        }
    }
    lookup
}

/// Resolve guild color by exact-name first, then normalized-name fallback.
pub fn lookup_guild_color(
    colors: &GuildColorMap,
    normalized_lookup: &GuildColorMap,
    guild_name: &str,
) -> Option<GuildColor> {
    colors.get(guild_name).copied().or_else(|| {
        normalize_guild_color_key(guild_name).and_then(|key| normalized_lookup.get(&key).copied())
    })
}

/// Pre-serialized SSE event — serialized once by the poller, shared by all clients via Arc.
#[derive(Debug, Clone)]
pub enum PreSerializedEvent {
    Snapshot { seq: u64, json: Arc<Bytes> },
    Update { seq: u64, json: Arc<Bytes> },
    RuntimeUpdate { seq: u64, json: Arc<Bytes> },
}

#[derive(Debug, Clone, Default)]
pub struct IngestTerritoryOverride {
    pub guild: Option<GuildRef>,
    pub acquired: Option<DateTime<Utc>>,
    pub runtime: Option<TerritoryRuntimeData>,
    pub observed_at: DateTime<Utc>,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub seq: u64,
    pub timestamp: String,
    pub territories: TerritoryMap,
    pub snapshot_json: Arc<Bytes>,
    pub territories_json: Arc<Bytes>,
    pub live_state_json: Arc<Bytes>,
    pub ownership_json: Arc<Bytes>,
}

impl Default for LiveSnapshot {
    fn default() -> Self {
        let seq = 0;
        let timestamp = Utc::now().to_rfc3339();
        let territories = TerritoryMap::new();
        let live_state_json = serde_json::to_vec(&LiveState {
            seq,
            timestamp: timestamp.clone(),
            territories: territories.clone(),
        })
        .map(Bytes::from)
        .unwrap_or_else(|_| Bytes::from_static(br#"{"seq":0,"timestamp":"","territories":{}}"#));

        Self {
            seq,
            timestamp,
            territories,
            snapshot_json: Arc::new(Bytes::new()),
            territories_json: Arc::new(Bytes::from_static(b"{}")),
            live_state_json: Arc::new(live_state_json),
            ownership_json: Arc::new(Bytes::from_static(b"{}")),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtraTerrInfo {
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub connections: Vec<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub live_snapshot: Arc<RwLock<LiveSnapshot>>,
    pub next_seq: Arc<AtomicU64>,
    pub next_seq_reserved: Arc<AtomicU64>,
    pub event_tx: broadcast::Sender<PreSerializedEvent>,
    pub guild_cache: Arc<DashMap<String, CachedGuild>>,
    /// Extra territory data (resources, connections) from supplemental gist.
    pub extra_terr: Arc<RwLock<HashMap<String, ExtraTerrInfo>>>,
    pub extra_data_dirty: Arc<AtomicBool>,
    /// Guild name -> RGB color from Athena/Wynntils.
    pub guild_colors: Arc<RwLock<GuildColorMap>>,
    pub guild_colors_dirty: Arc<AtomicBool>,
    /// Canonical territory overrides from ingest service (ownership/runtime).
    pub ingest_overrides: Arc<RwLock<HashMap<String, IngestTerritoryOverride>>>,
    /// Latest computed season scalar sample and pre-serialized API payload.
    pub latest_scalar_sample: Arc<RwLock<Option<CachedScalarSample>>>,
    pub http_client: reqwest::Client,
    /// PostgreSQL pool for history persistence. None if DATABASE_URL is not set.
    pub db: Option<PgPool>,
    pub seq_live_handoff_v1: bool,
    pub internal_ingest_token: Option<String>,
    pub max_ingest_updates_per_request: usize,
    pub max_history_replay_events: i64,
    pub max_history_sr_sample_rows: i64,
    pub observability: Arc<ObservabilityCounters>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGuild {
    pub data: String,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct ObservabilityCounters {
    live_state_requests_total: AtomicU64,
    persist_failures_total: AtomicU64,
    dropped_update_events_total: AtomicU64,
    persisted_update_events_total: AtomicU64,
    guilds_online_requests_total: AtomicU64,
    guilds_online_cache_hits_total: AtomicU64,
    guilds_online_cache_misses_total: AtomicU64,
    guilds_online_upstream_errors_total: AtomicU64,
    ingest_reports_total: AtomicU64,
    ingest_reports_rejected_total: AtomicU64,
    ingest_reports_applied_total: AtomicU64,
    ingest_reports_degraded_total: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
pub struct ObservabilitySnapshot {
    pub live_state_requests_total: u64,
    pub persist_failures_total: u64,
    pub dropped_update_events_total: u64,
    pub persisted_update_events_total: u64,
    pub guilds_online_requests_total: u64,
    pub guilds_online_cache_hits_total: u64,
    pub guilds_online_cache_misses_total: u64,
    pub guilds_online_upstream_errors_total: u64,
    pub ingest_reports_total: u64,
    pub ingest_reports_rejected_total: u64,
    pub ingest_reports_applied_total: u64,
    pub ingest_reports_degraded_total: u64,
}

impl ObservabilityCounters {
    pub fn snapshot(&self) -> ObservabilitySnapshot {
        ObservabilitySnapshot {
            live_state_requests_total: self.live_state_requests_total.load(Ordering::Relaxed),
            persist_failures_total: self.persist_failures_total.load(Ordering::Relaxed),
            dropped_update_events_total: self.dropped_update_events_total.load(Ordering::Relaxed),
            persisted_update_events_total: self
                .persisted_update_events_total
                .load(Ordering::Relaxed),
            guilds_online_requests_total: self.guilds_online_requests_total.load(Ordering::Relaxed),
            guilds_online_cache_hits_total: self
                .guilds_online_cache_hits_total
                .load(Ordering::Relaxed),
            guilds_online_cache_misses_total: self
                .guilds_online_cache_misses_total
                .load(Ordering::Relaxed),
            guilds_online_upstream_errors_total: self
                .guilds_online_upstream_errors_total
                .load(Ordering::Relaxed),
            ingest_reports_total: self.ingest_reports_total.load(Ordering::Relaxed),
            ingest_reports_rejected_total: self
                .ingest_reports_rejected_total
                .load(Ordering::Relaxed),
            ingest_reports_applied_total: self.ingest_reports_applied_total.load(Ordering::Relaxed),
            ingest_reports_degraded_total: self
                .ingest_reports_degraded_total
                .load(Ordering::Relaxed),
        }
    }

    pub fn record_live_state_request(&self) {
        self.live_state_requests_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_persist_failure(&self) {
        self.persist_failures_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dropped_update_events(&self, count: u64) {
        self.dropped_update_events_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_persisted_update_events(&self, count: u64) {
        self.persisted_update_events_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_guilds_online_request(&self) {
        self.guilds_online_requests_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_guilds_online_cache_hits(&self, count: u64) {
        self.guilds_online_cache_hits_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_guilds_online_cache_misses(&self, count: u64) {
        self.guilds_online_cache_misses_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_guilds_online_upstream_errors(&self, count: u64) {
        self.guilds_online_upstream_errors_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_ingest_reports(&self, count: u64) {
        self.ingest_reports_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_ingest_reports_rejected(&self, count: u64) {
        self.ingest_reports_rejected_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_ingest_reports_applied(&self, count: u64) {
        self.ingest_reports_applied_total
            .fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_ingest_reports_degraded(&self, count: u64) {
        self.ingest_reports_degraded_total
            .fetch_add(count, Ordering::Relaxed);
    }
}

impl AppState {
    pub fn new(db: Option<PgPool>) -> Self {
        let (event_tx, _) = broadcast::channel(sse_broadcast_buffer());
        let request_timeout = upstream_http_timeout();
        let connect_timeout = upstream_connect_timeout();
        let internal_ingest_token = internal_ingest_token();
        let has_configured_internal_token = std::env::var("INTERNAL_INGEST_TOKEN")
            .or_else(|_| std::env::var("internal_ingest_token"))
            .ok()
            .is_some();
        if has_configured_internal_token && internal_ingest_token.is_none() {
            warn!(
                "internal ingest token is configured but invalid; token must be non-empty, sufficiently long, and not a known placeholder"
            );
        }
        let http_client = reqwest::Client::builder()
            .user_agent("sequoia-map/0.1")
            .timeout(request_timeout)
            .connect_timeout(connect_timeout)
            .build()
            .or_else(|e| {
                warn!(
                    error = %e,
                    "failed to build configured HTTP client, retrying without custom user-agent"
                );
                reqwest::Client::builder()
                    .timeout(request_timeout)
                    .connect_timeout(connect_timeout)
                    .build()
            })
            .unwrap_or_else(|e| {
                panic!("failed to build timeout-configured HTTP client: {e}");
            });
        Self {
            live_snapshot: Arc::new(RwLock::new(LiveSnapshot::default())),
            next_seq: Arc::new(AtomicU64::new(0)),
            next_seq_reserved: Arc::new(AtomicU64::new(0)),
            event_tx,
            guild_cache: Arc::new(DashMap::new()),
            extra_terr: Arc::new(RwLock::new(HashMap::new())),
            extra_data_dirty: Arc::new(AtomicBool::new(true)),
            guild_colors: Arc::new(RwLock::new(HashMap::new())),
            guild_colors_dirty: Arc::new(AtomicBool::new(true)),
            ingest_overrides: Arc::new(RwLock::new(HashMap::new())),
            latest_scalar_sample: Arc::new(RwLock::new(None)),
            http_client,
            db,
            seq_live_handoff_v1: seq_live_handoff_v1_enabled(),
            internal_ingest_token,
            max_ingest_updates_per_request: max_ingest_updates_per_request(),
            max_history_replay_events: max_history_replay_events(),
            max_history_sr_sample_rows: max_history_sr_sample_rows(),
            observability: Arc::new(ObservabilityCounters::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{build_guild_color_lookup, lookup_guild_color, normalize_guild_color_key};

    #[test]
    fn normalize_guild_color_key_collapses_spaces_and_lowercases() {
        assert_eq!(
            normalize_guild_color_key("  AviCIA\t  Guild  "),
            Some("avicia guild".to_string())
        );
    }

    #[test]
    fn lookup_guild_color_uses_normalized_fallback() {
        let mut colors = HashMap::new();
        colors.insert("Avicia".to_string(), (16, 16, 254));
        let normalized = build_guild_color_lookup(&colors);

        assert_eq!(
            lookup_guild_color(&colors, &normalized, "  avicia  "),
            Some((16, 16, 254))
        );
        assert_eq!(
            lookup_guild_color(&colors, &normalized, "AVICIA"),
            Some((16, 16, 254))
        );
    }
}
