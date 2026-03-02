use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use ipnet::IpNet;
use reqwest::Client;
use sequoia_shared::{
    CanonicalTerritoryBatch, CanonicalTerritoryUpdate, DataProvenance, VisibilityClass,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
extern crate self as sqlx;
pub use sqlx_core::error::Error;
pub use sqlx_core::query::query;
pub use sqlx_core::query_as::query_as;
pub use sqlx_sqlite::{Sqlite, SqlitePool, SqlitePoolOptions};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

const RATE_WINDOW: Duration = Duration::from_secs(60);
const QUORUM_WINDOW: Duration = Duration::from_secs(120);
const ACTIVE_REPORTER_WINDOW: Duration = Duration::from_secs(180);
const TOKEN_TTL_HOURS: i64 = 24;
const TOKEN_ROTATE_MARGIN_MINS: i64 = 15;
const DEFAULT_INGEST_API_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_REPORTS_PER_BATCH: usize = 1024;
const DEFAULT_MAX_TERRITORY_NAME_LEN: usize = 96;
const DEFAULT_MAX_IDEMPOTENCY_KEY_LEN: usize = 128;

#[derive(Clone)]
struct Config {
    bind_addr: String,
    db_url: String,
    sequoia_server_base_url: String,
    internal_ingest_token: String,
    api_body_limit_bytes: usize,
    max_reporters: usize,
    rate_limit_ip_per_min: usize,
    rate_limit_reporter_per_min: usize,
    max_rate_limit_keys: usize,
    quorum_min_reporters: usize,
    degraded_single_reporter_enabled: bool,
    raw_retention_days: i64,
    reporter_retention_days: i64,
    duplicate_ttl_secs: u64,
    max_seen_idempotency_keys: usize,
    max_reports_per_batch: usize,
    max_territory_name_len: usize,
    max_idempotency_key_len: usize,
    malformed_threshold: u32,
    max_malformed_penalty_keys: usize,
    quarantine_secs: u64,
    max_pending_territories: usize,
    max_claims_per_territory: usize,
    max_forward_queue: usize,
    forward_max_attempts: u32,
    trusted_proxy_cidrs: Vec<IpNet>,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind_addr: std::env::var("SEQUOIA_INGEST_BIND")
                .unwrap_or_else(|_| "0.0.0.0:3010".to_string()),
            db_url: std::env::var("SEQUOIA_INGEST_DB_URL")
                .unwrap_or_else(|_| "sqlite://./sequoia-ingest.db".to_string()),
            sequoia_server_base_url: std::env::var("SEQUOIA_SERVER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string()),
            internal_ingest_token: load_internal_ingest_token()?,
            api_body_limit_bytes: std::env::var("INGEST_API_BODY_LIMIT_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_INGEST_API_BODY_LIMIT_BYTES),
            max_reporters: std::env::var("INGEST_MAX_REPORTERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            rate_limit_ip_per_min: std::env::var("INGEST_RATE_LIMIT_IP_PER_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            rate_limit_reporter_per_min: std::env::var("INGEST_RATE_LIMIT_REPORTER_PER_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            max_rate_limit_keys: std::env::var("INGEST_MAX_RATE_LIMIT_KEYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20_000),
            quorum_min_reporters: std::env::var("INGEST_QUORUM_MIN_REPORTERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            degraded_single_reporter_enabled: std::env::var(
                "INGEST_DEGRADED_SINGLE_REPORTER_ENABLED",
            )
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false),
            raw_retention_days: std::env::var("INGEST_RAW_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            reporter_retention_days: std::env::var("INGEST_REPORTER_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            duplicate_ttl_secs: std::env::var("INGEST_DUP_SUPPRESS_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            max_seen_idempotency_keys: std::env::var("INGEST_MAX_SEEN_IDEMPOTENCY_KEYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100_000),
            max_reports_per_batch: std::env::var("INGEST_MAX_REPORTS_PER_BATCH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_REPORTS_PER_BATCH),
            max_territory_name_len: std::env::var("INGEST_MAX_TERRITORY_NAME_LEN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_TERRITORY_NAME_LEN),
            max_idempotency_key_len: std::env::var("INGEST_MAX_IDEMPOTENCY_KEY_LEN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_IDEMPOTENCY_KEY_LEN),
            malformed_threshold: std::env::var("INGEST_MALFORMED_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            max_malformed_penalty_keys: std::env::var("INGEST_MAX_MALFORMED_PENALTY_KEYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20_000),
            quarantine_secs: std::env::var("INGEST_QUARANTINE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            max_pending_territories: std::env::var("INGEST_MAX_PENDING_TERRITORIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2_048),
            max_claims_per_territory: std::env::var("INGEST_MAX_CLAIMS_PER_TERRITORY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(64),
            max_forward_queue: std::env::var("INGEST_MAX_FORWARD_QUEUE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2_048),
            forward_max_attempts: std::env::var("INGEST_FORWARD_MAX_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            trusted_proxy_cidrs: parse_trusted_proxy_cidrs(
                &std::env::var("INGEST_TRUSTED_PROXY_CIDRS").unwrap_or_default(),
            ),
        })
    }
}

fn load_internal_ingest_token() -> anyhow::Result<String> {
    use anyhow::{Context, bail};

    let raw = std::env::var("SEQUOIA_INTERNAL_INGEST_TOKEN")
        .or_else(|_| std::env::var("INTERNAL_INGEST_TOKEN"))
        .context("missing required internal ingest token env var (SEQUOIA_INTERNAL_INGEST_TOKEN or INTERNAL_INGEST_TOKEN)")?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        bail!("internal ingest token must not be empty");
    }
    let lower = token.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "dev-internal-ingest-token" | "change-me" | "changeme" | "replace-with-long-random-token"
    ) {
        bail!("refusing placeholder internal ingest token value");
    }
    if token.len() < 24 {
        bail!("internal ingest token is too short; use at least 24 characters");
    }
    Ok(token)
}

fn parse_trusted_proxy_cidrs(raw: &str) -> Vec<IpNet> {
    raw.split(',')
        .filter_map(|part| {
            let cidr = part.trim();
            if cidr.is_empty() {
                return None;
            }
            match cidr.parse::<IpNet>() {
                Ok(parsed) => Some(parsed),
                Err(err) => {
                    warn!(cidr, error = %err, "ignoring invalid INGEST_TRUSTED_PROXY_CIDRS entry");
                    None
                }
            }
        })
        .collect()
}

#[derive(Default)]
struct Metrics {
    enrolled_total: AtomicU64,
    reports_accepted_total: AtomicU64,
    reports_rejected_total: AtomicU64,
    reports_degraded_total: AtomicU64,
    reports_quorum_total: AtomicU64,
    forward_failures_total: AtomicU64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ReporterFieldToggles {
    #[serde(default = "default_true")]
    share_owner: bool,
    #[serde(default = "default_true")]
    share_headquarters: bool,
    #[serde(default = "default_true")]
    share_held_resources: bool,
    #[serde(default = "default_true")]
    share_production_rates: bool,
    #[serde(default = "default_true")]
    share_storage_capacity: bool,
    #[serde(default = "default_true")]
    share_defense_tier: bool,
    #[serde(default = "default_true")]
    share_trading_routes: bool,
}

impl Default for ReporterFieldToggles {
    fn default() -> Self {
        Self {
            share_owner: true,
            share_headquarters: true,
            share_held_resources: true,
            share_production_rates: true,
            share_storage_capacity: true,
            share_defense_tier: true,
            share_trading_routes: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
struct ReporterRecord {
    token_hash: String,
    token_expires_at: DateTime<Utc>,
    revoked: bool,
    guild_opt_in: bool,
    field_toggles: ReporterFieldToggles,
    last_seen: DateTime<Utc>,
}

#[derive(Clone)]
struct ForwardJob {
    route: &'static str,
    payload: serde_json::Value,
    attempts: u32,
    next_attempt_at: Instant,
}

#[derive(Clone)]
struct PendingTerritoryClaim {
    reporter_id: String,
    origin_ip: IpAddr,
    claim_hash: String,
    update: CanonicalTerritoryUpdate,
    received_at: Instant,
}

#[derive(Clone)]
struct AppState {
    cfg: Config,
    db: SqlitePool,
    http: Client,
    reporters: Arc<RwLock<HashMap<String, ReporterRecord>>>,
    token_index: Arc<RwLock<HashMap<String, String>>>,
    ip_windows: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    reporter_windows: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    malformed_penalties: Arc<RwLock<HashMap<String, u32>>>,
    quarantined_until: Arc<RwLock<HashMap<String, Instant>>>,
    seen_idempotency: Arc<RwLock<HashMap<String, Instant>>>,
    pending_territory: Arc<RwLock<HashMap<String, Vec<PendingTerritoryClaim>>>>,
    forward_queue: Arc<RwLock<VecDeque<ForwardJob>>>,
    metrics: Arc<Metrics>,
}

#[derive(Debug, Deserialize)]
struct EnrollRequest {
    // Backward-compatible no-op kept for one phase.
    #[serde(default)]
    guild_opt_in: Option<bool>,
    #[serde(default)]
    field_toggles: Option<ReporterFieldToggles>,
    #[serde(default)]
    minecraft_version: Option<String>,
    #[serde(default)]
    mod_version: Option<String>,
}

#[derive(Debug, Serialize)]
struct EnrollResponse {
    ok: bool,
    reporter_id: String,
    token: String,
    token_expires_at: String,
    guild_opt_in: bool,
    field_toggles: ReporterFieldToggles,
}

#[derive(Debug, Deserialize)]
struct HeartbeatRequest {
    // Backward-compatible no-op kept for one phase.
    #[serde(default)]
    guild_opt_in: Option<bool>,
    #[serde(default)]
    field_toggles: Option<ReporterFieldToggles>,
}

#[derive(Debug, Serialize)]
struct HeartbeatResponse {
    ok: bool,
    reporter_id: String,
    token_expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotated_token: Option<String>,
    guild_opt_in: bool,
    field_toggles: ReporterFieldToggles,
}

#[derive(Debug, Serialize)]
struct ReportAck {
    ok: bool,
    accepted: u64,
    rejected: u64,
    degraded: u64,
    quorum: u64,
}

#[derive(Debug, Clone)]
struct AuthedReporter {
    reporter_id: String,
    field_toggles: ReporterFieldToggles,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("sequoia_ingest=info")),
        )
        .init();

    let cfg = Config::from_env()?;
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&cfg.db_url)
        .await?;
    initialize_db(&db).await?;

    let state = AppState {
        cfg: cfg.clone(),
        db,
        http: Client::builder().timeout(Duration::from_secs(10)).build()?,
        reporters: Arc::new(RwLock::new(HashMap::new())),
        token_index: Arc::new(RwLock::new(HashMap::new())),
        ip_windows: Arc::new(RwLock::new(HashMap::new())),
        reporter_windows: Arc::new(RwLock::new(HashMap::new())),
        malformed_penalties: Arc::new(RwLock::new(HashMap::new())),
        quarantined_until: Arc::new(RwLock::new(HashMap::new())),
        seen_idempotency: Arc::new(RwLock::new(HashMap::new())),
        pending_territory: Arc::new(RwLock::new(HashMap::new())),
        forward_queue: Arc::new(RwLock::new(VecDeque::new())),
        metrics: Arc::new(Metrics::default()),
    };

    bootstrap_reporters(&state).await?;

    spawn_retention_task(state.clone());
    spawn_forwarder_task(state.clone());

    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/enroll", post(enroll))
        .route("/v1/report/territory", post(report_territory))
        .route("/v1/heartbeat", post(heartbeat))
        .layer(DefaultBodyLimit::max(cfg.api_body_limit_bytes))
        .with_state(Arc::new(state));

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    info!(bind = %cfg.bind_addr, "sequoia ingest listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "timestamp": Utc::now().to_rfc3339(),
    }))
}

async fn metrics(State(state): State<Arc<AppState>>) -> String {
    format!(
        "# TYPE sequoia_ingest_enrolled_total counter\nsequoia_ingest_enrolled_total {}\n\
# TYPE sequoia_ingest_reports_accepted_total counter\nsequoia_ingest_reports_accepted_total {}\n\
# TYPE sequoia_ingest_reports_rejected_total counter\nsequoia_ingest_reports_rejected_total {}\n\
# TYPE sequoia_ingest_reports_degraded_total counter\nsequoia_ingest_reports_degraded_total {}\n\
# TYPE sequoia_ingest_reports_quorum_total counter\nsequoia_ingest_reports_quorum_total {}\n\
# TYPE sequoia_ingest_forward_failures_total counter\nsequoia_ingest_forward_failures_total {}\n",
        state.metrics.enrolled_total.load(Ordering::Relaxed),
        state.metrics.reports_accepted_total.load(Ordering::Relaxed),
        state.metrics.reports_rejected_total.load(Ordering::Relaxed),
        state.metrics.reports_degraded_total.load(Ordering::Relaxed),
        state.metrics.reports_quorum_total.load(Ordering::Relaxed),
        state.metrics.forward_failures_total.load(Ordering::Relaxed),
    )
}

async fn enroll(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, StatusCode> {
    let ip = resolve_client_ip(&headers, addr, &state.cfg.trusted_proxy_cidrs).to_string();
    if !check_rate_limit_ip(&state, &ip).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let authed_reporter = authenticate(&state, &headers)
        .await
        .ok()
        .map(|authed| authed.reporter_id);
    let reporter_id = authed_reporter.unwrap_or_else(|| Uuid::new_v4().to_string());
    let token = new_token();
    let token_hash = token_hash(&token);
    let now = Utc::now();
    let token_expires_at = now + chrono::TimeDelta::hours(TOKEN_TTL_HOURS);
    let guild_opt_in = req.guild_opt_in.unwrap_or(false);
    let field_toggles = req.field_toggles.unwrap_or_default();

    {
        let mut reporters = state.reporters.write().await;
        let mut token_index = state.token_index.write().await;

        let is_new_reporter = !reporters.contains_key(&reporter_id);
        if is_new_reporter && reporters.len() >= state.cfg.max_reporters {
            warn!(
                reporter_count = reporters.len(),
                max_reporters = state.cfg.max_reporters,
                "rejecting enrollment because reporter registry reached capacity"
            );
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }

        if let Some(existing) = reporters.get(&reporter_id) {
            token_index.remove(&existing.token_hash);
        }

        let record = ReporterRecord {
            token_hash: token_hash.clone(),
            token_expires_at,
            revoked: false,
            guild_opt_in,
            field_toggles: field_toggles.clone(),
            last_seen: now,
        };
        reporters.insert(reporter_id.clone(), record);
        token_index.insert(token_hash.clone(), reporter_id.clone());
    }

    persist_reporter(
        &state,
        &reporter_id,
        &token_hash,
        token_expires_at,
        guild_opt_in,
        &field_toggles,
        now,
        false,
    )
    .await;

    state.metrics.enrolled_total.fetch_add(1, Ordering::Relaxed);

    if let Some(mc) = req.minecraft_version.as_deref() {
        info!(reporter_id = %reporter_id, minecraft_version = %mc, "reporter enrolled");
    }
    if let Some(mod_ver) = req.mod_version.as_deref() {
        info!(reporter_id = %reporter_id, mod_version = %mod_ver, "reporter mod metadata");
    }

    Ok(Json(EnrollResponse {
        ok: true,
        reporter_id,
        token,
        token_expires_at: token_expires_at.to_rfc3339(),
        guild_opt_in,
        field_toggles,
    }))
}

async fn heartbeat(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<HeartbeatRequest>,
) -> Result<Json<HeartbeatResponse>, StatusCode> {
    let ip = resolve_client_ip(&headers, addr, &state.cfg.trusted_proxy_cidrs).to_string();
    let authed = authenticate(&state, &headers).await?;

    if !check_rate_limit_ip(&state, &ip).await
        || !check_rate_limit_reporter(&state, &authed.reporter_id).await
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    if is_quarantined(&state, &ip).await || is_quarantined(&state, &authed.reporter_id).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let now = Utc::now();
    let (token_expires_at, guild_opt_in, field_toggles, rotated_token, token_hash_to_persist) = {
        let mut reporters = state.reporters.write().await;
        let mut token_index = state.token_index.write().await;
        let Some(record) = reporters.get_mut(&authed.reporter_id) else {
            return Err(StatusCode::UNAUTHORIZED);
        };

        record.last_seen = now;
        if let Some(opt_in) = req.guild_opt_in {
            record.guild_opt_in = opt_in;
        }
        if let Some(toggles) = req.field_toggles.clone() {
            record.field_toggles = toggles;
        }

        let guild_opt_in = record.guild_opt_in;
        let field_toggles = record.field_toggles.clone();
        let mut token_expires_at = record.token_expires_at;
        let mut rotated_token = None;

        if (record.token_expires_at - now).num_minutes() <= TOKEN_ROTATE_MARGIN_MINS {
            let new = new_token();
            let new_hash = token_hash(&new);
            token_index.remove(&record.token_hash);
            record.token_hash = new_hash.clone();
            record.token_expires_at = now + chrono::TimeDelta::hours(TOKEN_TTL_HOURS);
            token_expires_at = record.token_expires_at;
            token_index.insert(new_hash, authed.reporter_id.clone());
            rotated_token = Some(new);
        }
        (
            token_expires_at,
            guild_opt_in,
            field_toggles,
            rotated_token,
            record.token_hash.clone(),
        )
    };

    persist_reporter(
        &state,
        &authed.reporter_id,
        &token_hash_to_persist,
        token_expires_at,
        guild_opt_in,
        &field_toggles,
        now,
        false,
    )
    .await;

    Ok(Json(HeartbeatResponse {
        ok: true,
        reporter_id: authed.reporter_id,
        token_expires_at: token_expires_at.to_rfc3339(),
        rotated_token,
        guild_opt_in,
        field_toggles,
    }))
}

async fn report_territory(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(mut batch): Json<CanonicalTerritoryBatch>,
) -> Result<Json<ReportAck>, StatusCode> {
    let client_ip = resolve_client_ip(&headers, addr, &state.cfg.trusted_proxy_cidrs);
    let ip = client_ip.to_string();
    let authed = authenticate(&state, &headers).await?;

    if !check_rate_limit_ip(&state, &ip).await
        || !check_rate_limit_reporter(&state, &authed.reporter_id).await
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    if is_quarantined(&state, &ip).await || is_quarantined(&state, &authed.reporter_id).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if state.cfg.max_reports_per_batch > 0 && batch.updates.len() > state.cfg.max_reports_per_batch
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    {
        let mut reporters = state.reporters.write().await;
        if let Some(record) = reporters.get_mut(&authed.reporter_id) {
            record.last_seen = Utc::now();
        }
    }

    let mut accepted = 0_u64;
    let mut rejected = 0_u64;
    let mut degraded = 0_u64;
    let mut quorum = 0_u64;
    let mut canonical_updates = Vec::new();

    for update in batch.updates.drain(..) {
        let Some(mut update) = apply_toggle_policy(update, &authed.field_toggles) else {
            rejected += 1;
            continue;
        };

        let Some(normalized_territory) =
            normalize_territory_name(&update.territory, state.cfg.max_territory_name_len)
        else {
            rejected += 1;
            register_malformed(&state, &authed.reporter_id, &ip).await;
            continue;
        };
        update.territory = normalized_territory.to_string();

        let idempotency_key = match normalize_idempotency_key(
            update.idempotency_key.as_deref(),
            state.cfg.max_idempotency_key_len,
        ) {
            Some(Some(normalized)) => normalized,
            Some(None) => territory_idempotency_hash(&authed.reporter_id, &update),
            None => {
                rejected += 1;
                register_malformed(&state, &authed.reporter_id, &ip).await;
                continue;
            }
        };

        if !claim_idempotency_key(&state, &idempotency_key).await {
            rejected += 1;
            continue;
        }
        update.idempotency_key = Some(idempotency_key.clone());

        persist_raw_report(
            &state,
            "territory",
            &authed.reporter_id,
            &ip,
            &serde_json::to_value(&update).unwrap_or_default(),
        )
        .await;

        let decision =
            evaluate_territory_claim(&state, &authed.reporter_id, client_ip, update.clone()).await;
        if let Some((mut accepted_update, was_degraded, was_quorum)) = decision {
            if let Some(runtime) = accepted_update.runtime.as_mut() {
                let mut provenance = runtime
                    .provenance
                    .clone()
                    .unwrap_or_else(default_provenance);
                if provenance.source.trim().is_empty() {
                    provenance.source = "fabric_reporter".to_string();
                }
                if provenance.observed_at.trim().is_empty() {
                    provenance.observed_at = Utc::now().to_rfc3339();
                }
                if was_degraded {
                    provenance.confidence = provenance.confidence.min(0.55).max(0.35);
                } else if was_quorum {
                    provenance.confidence = provenance.confidence.max(0.75);
                }
                runtime.provenance = Some(provenance);
            }

            accepted += 1;
            if was_degraded {
                degraded += 1;
            }
            if was_quorum {
                quorum += 1;
            }
            canonical_updates.push(accepted_update);
        }
    }

    if !canonical_updates.is_empty() {
        enqueue_forward(
            &state,
            "/api/internal/ingest/territory",
            serde_json::to_value(CanonicalTerritoryBatch {
                generated_at: batch.generated_at,
                updates: canonical_updates,
            })
            .unwrap_or_else(
                |_| serde_json::json!({"generated_at": Utc::now().to_rfc3339(), "updates": []}),
            ),
        )
        .await;
    }

    state
        .metrics
        .reports_accepted_total
        .fetch_add(accepted, Ordering::Relaxed);
    state
        .metrics
        .reports_rejected_total
        .fetch_add(rejected, Ordering::Relaxed);
    state
        .metrics
        .reports_degraded_total
        .fetch_add(degraded, Ordering::Relaxed);
    state
        .metrics
        .reports_quorum_total
        .fetch_add(quorum, Ordering::Relaxed);

    Ok(Json(ReportAck {
        ok: true,
        accepted,
        rejected,
        degraded,
        quorum,
    }))
}

async fn authenticate(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<AuthedReporter, StatusCode> {
    let token = bearer_token(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let token_hash = token_hash(&token);

    let reporter_id = {
        let token_index = state.token_index.read().await;
        token_index.get(&token_hash).cloned()
    }
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = Utc::now();
    let (revoked, expired, field_toggles) = {
        let reporters = state.reporters.read().await;
        let Some(record) = reporters.get(&reporter_id) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        (
            record.revoked,
            record.token_expires_at <= now,
            record.field_toggles.clone(),
        )
    };

    if revoked || expired {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(AuthedReporter {
        reporter_id,
        field_toggles,
    })
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    let mut parts = raw.splitn(2, ' ');
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return None;
    }
    Some(token.trim().to_string())
}

fn normalize_territory_name(raw: &str, max_len: usize) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if max_len > 0 && trimmed.len() > max_len {
        return None;
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return None;
    }
    Some(trimmed)
}

fn normalize_idempotency_key(raw: Option<&str>, max_len: usize) -> Option<Option<String>> {
    let Some(raw) = raw else {
        return Some(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(None);
    }
    if max_len > 0 && trimmed.len() > max_len {
        return None;
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return None;
    }
    Some(Some(trimmed.to_string()))
}

fn resolve_client_ip(
    headers: &HeaderMap,
    direct_peer: SocketAddr,
    trusted_proxies: &[IpNet],
) -> IpAddr {
    let direct_ip = direct_peer.ip();
    if !is_trusted_proxy_ip(direct_ip, trusted_proxies) {
        return direct_ip;
    }

    // Trust chain processing:
    // 1) Read XFF hops if immediate peer is trusted.
    // 2) Append direct peer as the right-most hop.
    // 3) Walk right-to-left to find first non-trusted proxy address.
    // This avoids left-most spoofing when proxies append incoming XFF values.
    let mut hops = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| {
                    let part = part.trim();
                    if part.is_empty() {
                        None
                    } else {
                        part.parse::<IpAddr>().ok()
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    hops.push(direct_ip);

    for ip in hops.iter().rev().copied() {
        if !is_trusted_proxy_ip(ip, trusted_proxies) {
            return ip;
        }
    }

    direct_ip
}

fn is_trusted_proxy_ip(ip: IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies.iter().any(|cidr| cidr.contains(&ip))
}

async fn check_rate_limit_ip(state: &Arc<AppState>, ip: &str) -> bool {
    check_rate_limit(
        &state.ip_windows,
        ip,
        state.cfg.rate_limit_ip_per_min,
        state.cfg.max_rate_limit_keys,
        RATE_WINDOW,
    )
    .await
}

async fn check_rate_limit_reporter(state: &Arc<AppState>, reporter_id: &str) -> bool {
    check_rate_limit(
        &state.reporter_windows,
        reporter_id,
        state.cfg.rate_limit_reporter_per_min,
        state.cfg.max_rate_limit_keys,
        RATE_WINDOW,
    )
    .await
}

async fn check_rate_limit(
    windows: &Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    key: &str,
    limit: usize,
    max_keys: usize,
    window: Duration,
) -> bool {
    let now = Instant::now();
    let mut guard = windows.write().await;

    guard.retain(|_, value| {
        while let Some(front) = value.front() {
            if now.duration_since(*front) > window {
                value.pop_front();
            } else {
                break;
            }
        }
        !value.is_empty()
    });

    if !guard.contains_key(key)
        && max_keys > 0
        && guard.len() >= max_keys
        && let Some(oldest_key) = guard
            .iter()
            .filter_map(|(candidate, value)| value.front().map(|front| (candidate.clone(), *front)))
            .min_by_key(|(_, front)| *front)
            .map(|(candidate, _)| candidate)
    {
        guard.remove(&oldest_key);
    }

    let entry = guard.entry(key.to_string()).or_default();
    while let Some(front) = entry.front() {
        if now.duration_since(*front) > window {
            entry.pop_front();
        } else {
            break;
        }
    }
    if entry.len() >= limit {
        return false;
    }
    entry.push_back(now);
    true
}

async fn is_quarantined(state: &Arc<AppState>, key: &str) -> bool {
    let now = Instant::now();
    let mut quarantine = state.quarantined_until.write().await;
    if let Some(until) = quarantine.get(key) {
        if *until > now {
            return true;
        }
        quarantine.remove(key);
    }
    false
}

async fn register_malformed(state: &Arc<AppState>, reporter_id: &str, ip: &str) {
    let reporter_key = format!("reporter:{reporter_id}");
    let ip_key = format!("ip:{ip}");

    let mut penalties = state.malformed_penalties.write().await;
    if !penalties.contains_key(&reporter_key)
        && !penalties.contains_key(&ip_key)
        && penalties.len() >= state.cfg.max_malformed_penalty_keys
    {
        penalties.clear();
        warn!(
            max_keys = state.cfg.max_malformed_penalty_keys,
            "reset malformed penalty map after reaching capacity"
        );
    }

    let reporter_count = penalties
        .entry(reporter_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let reporter_count_value = *reporter_count;
    let ip_count = penalties
        .entry(ip_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let ip_count_value = *ip_count;

    if reporter_count_value >= state.cfg.malformed_threshold
        || ip_count_value >= state.cfg.malformed_threshold
    {
        drop(penalties);
        let until = Instant::now() + Duration::from_secs(state.cfg.quarantine_secs);
        let mut quarantine = state.quarantined_until.write().await;
        quarantine.insert(reporter_id.to_string(), until);
        quarantine.insert(ip.to_string(), until);
        warn!(reporter_id = %reporter_id, ip = %ip, "quarantined reporter/ip due to repeated malformed payloads");
    }
}

async fn claim_idempotency_key(state: &Arc<AppState>, key: &str) -> bool {
    let now = Instant::now();
    let ttl = Duration::from_secs(state.cfg.duplicate_ttl_secs);

    let mut seen = state.seen_idempotency.write().await;
    seen.retain(|_, expires| *expires > now);

    if !seen.contains_key(key)
        && state.cfg.max_seen_idempotency_keys > 0
        && seen.len() >= state.cfg.max_seen_idempotency_keys
        && let Some(oldest_key) = seen
            .iter()
            .min_by_key(|(_, expires)| **expires)
            .map(|(existing_key, _)| existing_key.clone())
    {
        seen.remove(&oldest_key);
    }

    if seen.contains_key(key) {
        return false;
    }

    seen.insert(key.to_string(), now + ttl);
    true
}

async fn evaluate_territory_claim(
    state: &Arc<AppState>,
    reporter_id: &str,
    origin_ip: IpAddr,
    update: CanonicalTerritoryUpdate,
) -> Option<(CanonicalTerritoryUpdate, bool, bool)> {
    let now = Instant::now();
    let claim_hash = territory_claim_hash(&update);
    let territory_name = update.territory.clone();

    let mut pending = state.pending_territory.write().await;
    if !pending.contains_key(&territory_name)
        && state.cfg.max_pending_territories > 0
        && pending.len() >= state.cfg.max_pending_territories
        && let Some(oldest_key) = pending
            .iter()
            .filter_map(|(territory, claims)| {
                claims
                    .iter()
                    .map(|claim| claim.received_at)
                    .min()
                    .map(|received_at| (territory.clone(), received_at))
            })
            .min_by_key(|(_, received_at)| *received_at)
            .map(|(territory, _)| territory)
    {
        pending.remove(&oldest_key);
        warn!(
            dropped_territory = %oldest_key,
            max_pending_territories = state.cfg.max_pending_territories,
            "pending territory claim map reached capacity; dropped oldest territory bucket"
        );
    }

    let bucket = pending.entry(territory_name.clone()).or_default();
    bucket.retain(|claim| now.duration_since(claim.received_at) <= QUORUM_WINDOW);
    if state.cfg.max_claims_per_territory > 0 && bucket.len() >= state.cfg.max_claims_per_territory
    {
        let drop_count = bucket.len() - state.cfg.max_claims_per_territory + 1;
        bucket.drain(0..drop_count);
        warn!(
            territory = %territory_name,
            dropped_claims = drop_count,
            max_claims_per_territory = state.cfg.max_claims_per_territory,
            "pending claim bucket reached capacity; dropped oldest claims"
        );
    }

    if bucket
        .iter()
        .any(|claim| claim.reporter_id == reporter_id && claim.claim_hash == claim_hash)
    {
        return None;
    }

    bucket.push(PendingTerritoryClaim {
        reporter_id: reporter_id.to_string(),
        origin_ip,
        claim_hash: claim_hash.clone(),
        update: update.clone(),
        received_at: now,
    });

    let mut reporters = HashSet::new();
    let mut origins = HashSet::new();
    for claim in bucket.iter().filter(|claim| claim.claim_hash == claim_hash) {
        reporters.insert(claim.reporter_id.clone());
        origins.insert(claim.origin_ip);
    }

    let distinct_reporters = reporters.len();
    let distinct_origins = origins.len();
    let corroborating = distinct_reporters.min(distinct_origins);
    let quorum_threshold = state.cfg.quorum_min_reporters.max(1);
    let quorum_ok = quorum_satisfied(distinct_reporters, distinct_origins, quorum_threshold);
    let active_reporters = active_reporter_count(state).await;
    let degraded_ok = !quorum_ok
        && state.cfg.degraded_single_reporter_enabled
        && active_reporters <= 1
        && distinct_reporters == 1
        && distinct_origins == 1;

    if quorum_ok || degraded_ok {
        if let Some(runtime) = bucket
            .iter()
            .rev()
            .find(|claim| claim.claim_hash == claim_hash)
            .map(|claim| claim.update.runtime.clone())
            .flatten()
        {
            let mut accepted = update.clone();
            let mut runtime = runtime;
            let mut provenance = runtime
                .provenance
                .clone()
                .unwrap_or_else(default_provenance);
            provenance.reporter_count = corroborating as u16;
            runtime.provenance = Some(provenance);
            accepted.runtime = Some(runtime);
            bucket.retain(|claim| claim.claim_hash != claim_hash);
            return Some((accepted, degraded_ok, quorum_ok));
        }

        bucket.retain(|claim| claim.claim_hash != claim_hash);
        return Some((update, degraded_ok, quorum_ok));
    }

    None
}

async fn active_reporter_count(state: &Arc<AppState>) -> usize {
    let now = Utc::now();
    let active_since = now
        - chrono::TimeDelta::seconds(
            i64::try_from(ACTIVE_REPORTER_WINDOW.as_secs()).unwrap_or(180),
        );
    let reporters = state.reporters.read().await;
    reporters
        .values()
        .filter(|record| !record.revoked)
        .filter(|record| record.last_seen >= active_since)
        .count()
}

fn apply_toggle_policy(
    mut update: CanonicalTerritoryUpdate,
    toggles: &ReporterFieldToggles,
) -> Option<CanonicalTerritoryUpdate> {
    if !toggles.share_owner {
        update.guild = None;
        update.acquired = None;
    }

    if !toggles.share_trading_routes {
        update.connections = None;
        if let Some(runtime) = update.runtime.as_mut()
            && let Some(extra_scrapes) = runtime.extra_scrapes.as_mut()
        {
            extra_scrapes.remove("trading_routes");
            if extra_scrapes.is_empty() {
                runtime.extra_scrapes = None;
            }
        }
    }

    if let Some(runtime) = update.runtime.as_mut() {
        if !toggles.share_headquarters {
            runtime.headquarters = None;
        }
        if !toggles.share_held_resources {
            runtime.held_resources = None;
        }
        if !toggles.share_production_rates {
            runtime.production_rates = None;
        }
        if !toggles.share_storage_capacity {
            runtime.storage_capacity = None;
        }
        if !toggles.share_defense_tier {
            runtime.defense_tier = None;
        }

        let has_scalar_menu_provenance = runtime
            .provenance
            .as_ref()
            .map(has_scalar_menu_provenance)
            .unwrap_or(false);
        let has_extra_scrapes = runtime
            .extra_scrapes
            .as_ref()
            .is_some_and(|entries| !entries.is_empty());

        if runtime.headquarters.is_none()
            && runtime.held_resources.is_none()
            && runtime.production_rates.is_none()
            && runtime.storage_capacity.is_none()
            && runtime.defense_tier.is_none()
            && !has_extra_scrapes
            && !has_scalar_menu_provenance
        {
            update.runtime = None;
        }
    }

    let has_payload = update.guild.is_some()
        || update.acquired.is_some()
        || update.location.is_some()
        || update.resources.is_some()
        || update.connections.is_some()
        || update.runtime.is_some();
    if !has_payload {
        return None;
    }

    Some(update)
}

fn default_provenance() -> DataProvenance {
    DataProvenance {
        source: "fabric_reporter".to_string(),
        visibility: VisibilityClass::Public,
        confidence: 0.5,
        reporter_count: 1,
        observed_at: Utc::now().to_rfc3339(),
        menu_season_id: None,
        menu_captured_territories: None,
        menu_sr_per_hour: None,
        menu_observed_at: None,
    }
}

fn has_scalar_menu_provenance(provenance: &DataProvenance) -> bool {
    provenance.menu_season_id.is_some()
        && provenance.menu_captured_territories.is_some()
        && provenance.menu_sr_per_hour.is_some()
}

fn quorum_satisfied(distinct_reporters: usize, distinct_origins: usize, threshold: usize) -> bool {
    let required = threshold.max(1);
    distinct_reporters >= required && distinct_origins >= required
}

fn canonicalize_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));

            let mut canonical = serde_json::Map::with_capacity(entries.len());
            for (key, child) in entries {
                canonical.insert(key, canonicalize_json_value(child));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(canonicalize_json_value)
                .collect::<Vec<_>>(),
        ),
        primitive => primitive,
    }
}

fn canonical_json_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_value(value)
        .ok()
        .map(canonicalize_json_value)
        .and_then(|canonical| serde_json::to_vec(&canonical).ok())
        .unwrap_or_default()
}

fn territory_claim_hash(update: &CanonicalTerritoryUpdate) -> String {
    let mut canonical = update.clone();
    canonical.idempotency_key = None;
    if let Some(runtime) = canonical.runtime.as_mut() {
        // Quorum should compare semantic claim data, not reporter-local metadata.
        runtime.provenance = None;
    }
    let payload = canonical_json_bytes(&canonical);
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    hex::encode(hasher.finalize())
}

fn territory_idempotency_hash(reporter_id: &str, update: &CanonicalTerritoryUpdate) -> String {
    let payload = canonical_json_bytes(update);
    let mut hasher = Sha256::new();
    hasher.update(reporter_id.as_bytes());
    hasher.update(&payload);
    hex::encode(hasher.finalize())
}

fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn normalize_persisted_token(token: &str) -> String {
    let trimmed = token.trim();
    if is_sha256_hex(trimmed) {
        return trimmed.to_ascii_lowercase();
    }
    token_hash(trimmed)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn new_token() -> String {
    format!("{}:{}", Uuid::new_v4(), Uuid::new_v4())
}

async fn enqueue_forward(state: &Arc<AppState>, route: &'static str, payload: serde_json::Value) {
    let mut queue = state.forward_queue.write().await;
    if state.cfg.max_forward_queue > 0 && queue.len() >= state.cfg.max_forward_queue {
        queue.pop_front();
        warn!(
            max_forward_queue = state.cfg.max_forward_queue,
            "forward queue reached capacity; dropped oldest queued job"
        );
    }
    queue.push_back(ForwardJob {
        route,
        payload,
        attempts: 0,
        next_attempt_at: Instant::now(),
    });
}

fn spawn_forwarder_task(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let mut maybe_job = None;
            {
                let mut queue = state.forward_queue.write().await;
                if let Some(index) = queue
                    .iter()
                    .position(|job| job.next_attempt_at <= Instant::now())
                {
                    maybe_job = queue.remove(index);
                }
            }

            let Some(mut job) = maybe_job else {
                continue;
            };

            let url = format!("{}{}", state.cfg.sequoia_server_base_url, job.route);
            let send = state
                .http
                .post(url)
                .header("x-internal-ingest-token", &state.cfg.internal_ingest_token)
                .json(&job.payload)
                .send()
                .await;

            match send {
                Ok(response) if response.status().is_success() => {
                    // delivered
                }
                Ok(response) => {
                    state
                        .metrics
                        .forward_failures_total
                        .fetch_add(1, Ordering::Relaxed);
                    let status = response.status();
                    warn!(status = %status, attempts = job.attempts, "forward request rejected");
                    schedule_retry(&state, &mut job).await;
                }
                Err(err) => {
                    state
                        .metrics
                        .forward_failures_total
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(error = %err, attempts = job.attempts, "forward request failed");
                    schedule_retry(&state, &mut job).await;
                }
            }
        }
    });
}

async fn schedule_retry(state: &AppState, job: &mut ForwardJob) {
    job.attempts = job.attempts.saturating_add(1);
    if job.attempts >= state.cfg.forward_max_attempts {
        warn!(
            route = job.route,
            attempts = job.attempts,
            max_attempts = state.cfg.forward_max_attempts,
            "dropping forward job after exhausting retry attempts"
        );
        return;
    }

    let backoff_secs = (1_u64 << job.attempts.min(6)).min(60);
    job.next_attempt_at = Instant::now() + Duration::from_secs(backoff_secs);

    let mut queue = state.forward_queue.write().await;
    if state.cfg.max_forward_queue > 0 && queue.len() >= state.cfg.max_forward_queue {
        queue.pop_front();
        warn!(
            max_forward_queue = state.cfg.max_forward_queue,
            "forward queue reached capacity during retry; dropped oldest queued job"
        );
    }
    queue.push_back(job.clone());
}

fn spawn_retention_task(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Err(err) = purge_expired(&state).await {
                warn!(error = %err, "retention purge failed");
            }
        }
    });
}

async fn purge_expired(state: &AppState) -> Result<(), String> {
    let cutoff = Utc::now() - chrono::TimeDelta::days(state.cfg.raw_retention_days);
    sqlx::query("DELETE FROM raw_reports WHERE received_at < ?")
        .bind(cutoff.to_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| format!("delete expired raw_reports: {e}"))?;

    let reporter_cutoff = Utc::now() - chrono::TimeDelta::days(state.cfg.reporter_retention_days);
    sqlx::query("DELETE FROM reporters WHERE last_seen < ?")
        .bind(reporter_cutoff.to_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| format!("delete expired reporters: {e}"))?;

    {
        let mut reporters = state.reporters.write().await;
        let mut token_index = state.token_index.write().await;
        let stale_reporters: Vec<String> = reporters
            .iter()
            .filter_map(|(reporter_id, record)| {
                (record.last_seen < reporter_cutoff).then_some(reporter_id.clone())
            })
            .collect();
        for reporter_id in stale_reporters {
            if let Some(record) = reporters.remove(&reporter_id) {
                token_index.remove(&record.token_hash);
            }
        }
    }

    let now = Instant::now();
    {
        let mut seen = state.seen_idempotency.write().await;
        seen.retain(|_, until| *until > now);
    }
    {
        let mut quarantine = state.quarantined_until.write().await;
        quarantine.retain(|_, until| *until > now);
    }
    {
        let mut windows = state.ip_windows.write().await;
        windows.retain(|_, bucket| !bucket.is_empty());
    }
    {
        let mut windows = state.reporter_windows.write().await;
        windows.retain(|_, bucket| !bucket.is_empty());
    }
    {
        let mut pending = state.pending_territory.write().await;
        pending.retain(|_, claims| !claims.is_empty());
    }
    {
        let mut penalties = state.malformed_penalties.write().await;
        if penalties.len() > state.cfg.max_malformed_penalty_keys {
            penalties.clear();
        }
    }

    Ok(())
}

async fn initialize_db(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS reporters (\
         reporter_id TEXT PRIMARY KEY,\
         token TEXT NOT NULL,\
         token_expires_at TEXT NOT NULL,\
         guild_opt_in INTEGER NOT NULL DEFAULT 0,\
         share_owner INTEGER NOT NULL DEFAULT 1,\
         share_headquarters INTEGER NOT NULL DEFAULT 1,\
         share_held_resources INTEGER NOT NULL DEFAULT 1,\
         share_production_rates INTEGER NOT NULL DEFAULT 1,\
         share_storage_capacity INTEGER NOT NULL DEFAULT 1,\
         share_defense_tier INTEGER NOT NULL DEFAULT 1,\
         share_trading_routes INTEGER NOT NULL DEFAULT 1,\
         revoked INTEGER NOT NULL DEFAULT 0,\
         last_seen TEXT NOT NULL,\
         created_at TEXT NOT NULL DEFAULT (datetime('now'))\
         )",
    )
    .execute(pool)
    .await?;

    // Backfill for older installations where these columns do not exist yet.
    sqlx::query("ALTER TABLE reporters ADD COLUMN share_owner INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN share_headquarters INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN share_held_resources INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await
        .ok();
    sqlx::query(
        "ALTER TABLE reporters ADD COLUMN share_production_rates INTEGER NOT NULL DEFAULT 1",
    )
    .execute(pool)
    .await
    .ok();
    sqlx::query(
        "ALTER TABLE reporters ADD COLUMN share_storage_capacity INTEGER NOT NULL DEFAULT 1",
    )
    .execute(pool)
    .await
    .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN share_defense_tier INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN share_trading_routes INTEGER NOT NULL DEFAULT 1")
        .execute(pool)
        .await
        .ok();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS raw_reports (\
         id INTEGER PRIMARY KEY AUTOINCREMENT,\
         kind TEXT NOT NULL,\
         reporter_id TEXT NOT NULL,\
         ip_address TEXT NOT NULL,\
         received_at TEXT NOT NULL,\
         payload TEXT NOT NULL\
         )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_raw_reports_received_at ON raw_reports (received_at)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn bootstrap_reporters(state: &AppState) -> Result<(), sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, i64, i64, i64, i64, i64, i64, i64, i64, String)>(
        "SELECT reporter_id, token, token_expires_at, guild_opt_in, \
                share_owner, share_headquarters, share_held_resources, \
                share_production_rates, share_storage_capacity, share_defense_tier, share_trading_routes, \
                revoked, last_seen \
         FROM reporters",
    )
    .fetch_all(&state.db)
    .await?;

    let mut reporters = state.reporters.write().await;
    let mut token_index = state.token_index.write().await;
    let mut migrated_plaintext_tokens = 0_usize;

    for (
        reporter_id,
        token,
        token_expires_at,
        guild_opt_in,
        share_owner,
        share_headquarters,
        share_held_resources,
        share_production_rates,
        share_storage_capacity,
        share_defense_tier,
        share_trading_routes,
        revoked,
        last_seen,
    ) in rows
    {
        let persisted_token_hash = normalize_persisted_token(&token);
        let token_expires_at = DateTime::parse_from_rfc3339(&token_expires_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() + chrono::TimeDelta::hours(TOKEN_TTL_HOURS));
        let last_seen = DateTime::parse_from_rfc3339(&last_seen)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let record = ReporterRecord {
            token_hash: persisted_token_hash.clone(),
            token_expires_at,
            revoked: revoked != 0,
            guild_opt_in: guild_opt_in != 0,
            field_toggles: ReporterFieldToggles {
                share_owner: share_owner != 0,
                share_headquarters: share_headquarters != 0,
                share_held_resources: share_held_resources != 0,
                share_production_rates: share_production_rates != 0,
                share_storage_capacity: share_storage_capacity != 0,
                share_defense_tier: share_defense_tier != 0,
                share_trading_routes: share_trading_routes != 0,
            },
            last_seen,
        };
        token_index.insert(persisted_token_hash.clone(), reporter_id.clone());
        reporters.insert(reporter_id, record);

        if persisted_token_hash != token {
            migrated_plaintext_tokens += 1;
        }
    }

    drop(token_index);
    drop(reporters);

    if migrated_plaintext_tokens > 0 {
        // Backward-compatible migration: rewrite legacy plaintext tokens as SHA-256 digests.
        let rows =
            sqlx::query_as::<_, (String, String)>("SELECT reporter_id, token FROM reporters")
                .fetch_all(&state.db)
                .await?;
        for (reporter_id, persisted) in rows {
            let hashed = normalize_persisted_token(&persisted);
            if hashed == persisted {
                continue;
            }
            if let Err(err) = sqlx::query("UPDATE reporters SET token = ? WHERE reporter_id = ?")
                .bind(&hashed)
                .bind(&reporter_id)
                .execute(&state.db)
                .await
            {
                warn!(
                    reporter_id = %reporter_id,
                    error = %err,
                    "failed to migrate legacy plaintext reporter token"
                );
            }
        }
    }

    let reporter_count = state.reporters.read().await.len();
    info!(
        reporters = reporter_count,
        migrated_plaintext_tokens, "loaded reporter registry"
    );
    Ok(())
}

async fn persist_reporter(
    state: &AppState,
    reporter_id: &str,
    token_hash: &str,
    token_expires_at: DateTime<Utc>,
    guild_opt_in: bool,
    field_toggles: &ReporterFieldToggles,
    last_seen: DateTime<Utc>,
    revoked: bool,
) {
    if let Err(err) = sqlx::query(
        "INSERT INTO reporters (reporter_id, token, token_expires_at, guild_opt_in, \
                               share_owner, share_headquarters, share_held_resources, \
                               share_production_rates, share_storage_capacity, share_defense_tier, share_trading_routes, \
                               revoked, last_seen) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(reporter_id) DO UPDATE SET \
           token=excluded.token, \
           token_expires_at=excluded.token_expires_at, \
           guild_opt_in=excluded.guild_opt_in, \
           share_owner=excluded.share_owner, \
           share_headquarters=excluded.share_headquarters, \
           share_held_resources=excluded.share_held_resources, \
           share_production_rates=excluded.share_production_rates, \
           share_storage_capacity=excluded.share_storage_capacity, \
           share_defense_tier=excluded.share_defense_tier, \
           share_trading_routes=excluded.share_trading_routes, \
           revoked=excluded.revoked, \
           last_seen=excluded.last_seen",
    )
    .bind(reporter_id)
    .bind(token_hash)
    .bind(token_expires_at.to_rfc3339())
    .bind(i64::from(guild_opt_in))
    .bind(i64::from(field_toggles.share_owner))
    .bind(i64::from(field_toggles.share_headquarters))
    .bind(i64::from(field_toggles.share_held_resources))
    .bind(i64::from(field_toggles.share_production_rates))
    .bind(i64::from(field_toggles.share_storage_capacity))
    .bind(i64::from(field_toggles.share_defense_tier))
    .bind(i64::from(field_toggles.share_trading_routes))
    .bind(i64::from(revoked))
    .bind(last_seen.to_rfc3339())
    .execute(&state.db)
    .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to persist reporter");
    }
}

async fn persist_raw_report(
    state: &AppState,
    kind: &str,
    reporter_id: &str,
    ip: &str,
    payload: &serde_json::Value,
) {
    if let Err(err) = sqlx::query(
        "INSERT INTO raw_reports (kind, reporter_id, ip_address, received_at, payload) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(kind)
    .bind(reporter_id)
    .bind(ip)
    .bind(Utc::now().to_rfc3339())
    .bind(payload.to_string())
    .execute(&state.db)
    .await
    {
        warn!(error = %err, kind = %kind, reporter_id = %reporter_id, "failed to persist raw report");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ReporterFieldToggles, apply_toggle_policy, normalize_idempotency_key,
        normalize_persisted_token, normalize_territory_name, parse_trusted_proxy_cidrs,
        quorum_satisfied, resolve_client_ip, territory_claim_hash, territory_idempotency_hash,
    };
    use axum::http::{HeaderMap, HeaderValue};
    use sequoia_shared::{CanonicalTerritoryUpdate, DataProvenance, TerritoryRuntimeData};
    use std::collections::HashMap;
    use std::net::{IpAddr, SocketAddr};

    fn runtime_with_scalar_provenance(
        observed_at: &str,
        menu_observed_at: &str,
        reporter_count: u16,
    ) -> TerritoryRuntimeData {
        TerritoryRuntimeData {
            headquarters: None,
            held_resources: None,
            production_rates: None,
            storage_capacity: None,
            defense_tier: None,
            contested: None,
            active_war: None,
            extra_scrapes: None,
            provenance: Some(DataProvenance {
                source: "fabric_reporter".to_string(),
                visibility: sequoia_shared::VisibilityClass::Public,
                confidence: 0.75,
                reporter_count,
                observed_at: observed_at.to_string(),
                menu_season_id: Some(29),
                menu_captured_territories: Some(65),
                menu_sr_per_hour: Some(30301),
                menu_observed_at: Some(menu_observed_at.to_string()),
            }),
        }
    }

    fn toggles_all_off() -> ReporterFieldToggles {
        ReporterFieldToggles {
            share_owner: false,
            share_headquarters: false,
            share_held_resources: false,
            share_production_rates: false,
            share_storage_capacity: false,
            share_defense_tier: false,
            share_trading_routes: false,
        }
    }

    #[test]
    fn territory_claim_hash_ignores_idempotency_and_provenance_metadata() {
        let first = CanonicalTerritoryUpdate {
            territory: "Ragni Plains".to_string(),
            guild: None,
            acquired: None,
            location: None,
            resources: None,
            connections: None,
            runtime: Some(runtime_with_scalar_provenance(
                "2026-02-28T20:00:00Z",
                "2026-02-28T19:59:58Z",
                1,
            )),
            idempotency_key: Some("a".to_string()),
        };
        let mut second = first.clone();
        second.idempotency_key = Some("b".to_string());
        if let Some(runtime) = second.runtime.as_mut()
            && let Some(provenance) = runtime.provenance.as_mut()
        {
            provenance.observed_at = "2026-02-28T20:01:00Z".to_string();
            provenance.menu_observed_at = Some("2026-02-28T20:00:59Z".to_string());
            provenance.reporter_count = 7;
        }

        let first_hash = territory_claim_hash(&first);
        let second_hash = territory_claim_hash(&second);
        assert_eq!(first_hash, second_hash);

        let mut changed = first.clone();
        if let Some(runtime) = changed.runtime.as_mut() {
            runtime.defense_tier = Some("Very High".to_string());
        }
        assert_ne!(territory_claim_hash(&first), territory_claim_hash(&changed));
        assert_ne!(
            territory_idempotency_hash("reporter-a", &first),
            territory_idempotency_hash("reporter-b", &first)
        );
    }

    #[test]
    fn territory_claim_hash_is_stable_for_semantically_equal_map_payloads() {
        let mut first_runtime =
            runtime_with_scalar_provenance("2026-02-28T20:00:00Z", "2026-02-28T19:59:58Z", 1);
        let mut first_extra_scrapes = HashMap::new();
        first_extra_scrapes.insert(
            "omega".to_string(),
            serde_json::json!({"z": 1, "a": [2, 3]}),
        );
        first_extra_scrapes.insert(
            "alpha".to_string(),
            serde_json::json!({"nested": {"k2": 2, "k1": 1}}),
        );
        first_runtime.extra_scrapes = Some(first_extra_scrapes);

        let mut second_runtime =
            runtime_with_scalar_provenance("2026-02-28T20:00:00Z", "2026-02-28T19:59:58Z", 1);
        let mut second_extra_scrapes = HashMap::new();
        second_extra_scrapes.insert(
            "alpha".to_string(),
            serde_json::json!({"nested": {"k1": 1, "k2": 2}}),
        );
        second_extra_scrapes.insert(
            "omega".to_string(),
            serde_json::json!({"a": [2, 3], "z": 1}),
        );
        second_runtime.extra_scrapes = Some(second_extra_scrapes);

        let first = CanonicalTerritoryUpdate {
            territory: "Ragni Plains".to_string(),
            guild: None,
            acquired: None,
            location: None,
            resources: None,
            connections: None,
            runtime: Some(first_runtime),
            idempotency_key: Some("id-a".to_string()),
        };
        let second = CanonicalTerritoryUpdate {
            territory: "Ragni Plains".to_string(),
            guild: None,
            acquired: None,
            location: None,
            resources: None,
            connections: None,
            runtime: Some(second_runtime),
            idempotency_key: Some("id-a".to_string()),
        };

        assert_eq!(territory_claim_hash(&first), territory_claim_hash(&second));
        assert_eq!(
            territory_idempotency_hash("reporter-a", &first),
            territory_idempotency_hash("reporter-a", &second)
        );
    }

    #[test]
    fn apply_toggle_policy_keeps_runtime_for_scalar_provenance_only_updates() {
        let update = CanonicalTerritoryUpdate {
            territory: "Ragni Plains".to_string(),
            guild: None,
            acquired: None,
            location: None,
            resources: None,
            connections: None,
            runtime: Some(runtime_with_scalar_provenance(
                "2026-02-28T20:00:00Z",
                "2026-02-28T19:59:58Z",
                1,
            )),
            idempotency_key: Some("id-1".to_string()),
        };

        let filtered = apply_toggle_policy(update, &toggles_all_off())
            .expect("scalar provenance update should remain after toggle filtering");
        assert!(filtered.runtime.is_some());
        assert!(
            filtered
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.provenance.as_ref())
                .is_some()
        );
    }

    #[test]
    fn resolve_client_ip_ignores_forwarded_for_from_untrusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));

        let resolved = resolve_client_ip(
            &headers,
            SocketAddr::from(([198, 51, 100, 20], 3010)),
            &parse_trusted_proxy_cidrs("10.0.0.0/8"),
        );

        assert_eq!(resolved, IpAddr::from([198, 51, 100, 20]));
    }

    #[test]
    fn resolve_client_ip_uses_forwarded_for_from_trusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.10, 10.0.0.8"),
        );

        let resolved = resolve_client_ip(
            &headers,
            SocketAddr::from(([10, 0, 0, 8], 3010)),
            &parse_trusted_proxy_cidrs("10.0.0.0/8"),
        );

        assert_eq!(resolved, IpAddr::from([203, 0, 113, 10]));
    }

    #[test]
    fn resolve_client_ip_rejects_leftmost_spoof_when_trusted_proxy_appends() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.99, 203.0.113.10"),
        );

        let resolved = resolve_client_ip(
            &headers,
            SocketAddr::from(([10, 0, 0, 8], 3010)),
            &parse_trusted_proxy_cidrs("10.0.0.0/8"),
        );

        assert_eq!(resolved, IpAddr::from([203, 0, 113, 10]));
    }

    #[test]
    fn resolve_client_ip_uses_xff_for_docker_proxy_defaults() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));

        let trusted = parse_trusted_proxy_cidrs(
            "127.0.0.1/32,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
        );
        let resolved = resolve_client_ip(
            &headers,
            SocketAddr::from(([172, 19, 0, 3], 3010)),
            &trusted,
        );

        assert_eq!(resolved, IpAddr::from([203, 0, 113, 10]));
    }

    #[test]
    fn parse_trusted_proxy_cidrs_skips_invalid_entries() {
        let parsed = parse_trusted_proxy_cidrs("10.0.0.0/8, not-a-cidr, 192.168.0.0/16");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn quorum_requires_distinct_reporters_and_origins() {
        assert!(!quorum_satisfied(2, 1, 2));
        assert!(!quorum_satisfied(1, 2, 2));
        assert!(quorum_satisfied(2, 2, 2));
    }

    #[test]
    fn normalize_persisted_token_hashes_plaintext_and_normalizes_digest_case() {
        let legacy_plaintext =
            "7f86b6d0-9f95-4b8d-a2bf-8100d3f4dbad:8ca1f91e-f788-4db0-9737-6f7d4f4e70ba";
        let hashed = normalize_persisted_token(legacy_plaintext);
        assert_eq!(hashed.len(), 64);
        assert_ne!(hashed, legacy_plaintext);
        assert!(hashed.chars().all(|ch| ch.is_ascii_hexdigit()));

        let uppercase_hash = "2BF2A8063077D3996A930476B1115D5A7E31EAF20E303A3A17DD1A79059722B4";
        let normalized = normalize_persisted_token(uppercase_hash);
        assert_eq!(
            normalized,
            "2bf2a8063077d3996a930476b1115d5a7e31eaf20e303a3a17dd1a79059722b4"
        );
    }

    #[test]
    fn normalize_territory_name_rejects_empty_control_or_too_long_values() {
        let long_name = "A".repeat(65);
        assert_eq!(
            normalize_territory_name("  Ragni Plains  ", 64),
            Some("Ragni Plains")
        );
        assert_eq!(normalize_territory_name("   ", 64), None);
        assert_eq!(normalize_territory_name("Ragni\u{0008} Plains", 64), None);
        assert_eq!(normalize_territory_name(&long_name, 64), None);
    }

    #[test]
    fn normalize_idempotency_key_trims_and_enforces_length() {
        let long_key = "A".repeat(129);
        assert_eq!(normalize_idempotency_key(None, 128), Some(None));
        assert_eq!(normalize_idempotency_key(Some("   "), 128), Some(None));
        assert_eq!(
            normalize_idempotency_key(Some("  key-123  "), 128),
            Some(Some("key-123".to_string()))
        );
        assert_eq!(normalize_idempotency_key(Some("bad\u{0000}key"), 128), None);
        assert_eq!(normalize_idempotency_key(Some(&long_key), 128), None);
    }
}
