use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::pkcs8::DecodePublicKey;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ipnet::IpNet;
use reqwest::Client;
use sequoia_shared::{
    CanonicalTerritoryBatch, CanonicalTerritoryUpdate, DataProvenance, TerritoryRuntimeData,
    VisibilityClass,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
extern crate self as sqlx;
pub use sqlx_core::error::Error;
pub use sqlx_core::query::query;
pub use sqlx_core::query_as::query_as;
use sqlx_core::row::Row;
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
const DEFAULT_SIGNED_NONCE_WINDOW_SECS: u64 = 300;
const DEFAULT_MAX_SIGNED_NONCE_KEYS: usize = 100_000;
const DEFAULT_WORLD_ATTESTATION_MAX_AGE_SECS: u64 = 120;
const DEFAULT_SESSION_REFRESH_INTERVAL_SECS: u64 = 600;
const DEFAULT_SESSION_FAIL_OPEN_GRACE_SECS: u64 = 1800;
const DEFAULT_OWNER_CORROBORATION_WINDOW_SECS: u64 = 90;
const DEFAULT_ACTIVE_REPORTER_STALE_SECS: u64 = 1800;
const CHALLENGE_TTL_SECS: u64 = 120;
const HDR_IRIS_KEY_ID: &str = "x-iris-key-id";
const HDR_IRIS_TS: &str = "x-iris-ts";
const HDR_IRIS_NONCE: &str = "x-iris-nonce";
const HDR_IRIS_SIG: &str = "x-iris-sig";

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
    auth_required: bool,
    single_reporter_mode: bool,
    require_session_proof: bool,
    session_refresh_interval_secs: u64,
    session_fail_open_grace_secs: u64,
    allowed_server_host_suffixes: Vec<String>,
    world_attestation_max_age_secs: u64,
    max_signed_nonce_keys: usize,
    signed_nonce_window_secs: u64,
    owner_soft_corroboration: bool,
    owner_corroboration_window_secs: u64,
    owner_revert_on_mismatch: bool,
    active_reporter_stale_secs: u64,
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
            auth_required: read_bool_env("INGEST_AUTH_REQUIRED", true),
            single_reporter_mode: read_bool_env("INGEST_SINGLE_REPORTER_MODE", true),
            require_session_proof: read_bool_env("INGEST_REQUIRE_SESSION_PROOF", true),
            session_refresh_interval_secs: std::env::var("INGEST_SESSION_REFRESH_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_SESSION_REFRESH_INTERVAL_SECS),
            session_fail_open_grace_secs: std::env::var("INGEST_SESSION_FAIL_OPEN_GRACE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_SESSION_FAIL_OPEN_GRACE_SECS),
            allowed_server_host_suffixes: parse_allowed_server_host_suffixes(
                &std::env::var("INGEST_ALLOWED_SERVER_HOST_SUFFIXES")
                    .unwrap_or_else(|_| ".wynncraft.com".to_string()),
            ),
            world_attestation_max_age_secs: std::env::var("INGEST_WORLD_ATTESTATION_MAX_AGE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_WORLD_ATTESTATION_MAX_AGE_SECS),
            max_signed_nonce_keys: std::env::var("INGEST_MAX_SIGNED_NONCE_KEYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_SIGNED_NONCE_KEYS),
            signed_nonce_window_secs: std::env::var("INGEST_SIGNED_NONCE_WINDOW_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_SIGNED_NONCE_WINDOW_SECS),
            owner_soft_corroboration: read_bool_env("INGEST_OWNER_SOFT_CORROBORATION", true),
            owner_corroboration_window_secs: std::env::var(
                "INGEST_OWNER_CORROBORATION_WINDOW_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_OWNER_CORROBORATION_WINDOW_SECS),
            owner_revert_on_mismatch: read_bool_env("INGEST_OWNER_REVERT_ON_MISMATCH", true),
            active_reporter_stale_secs: std::env::var("INGEST_ACTIVE_REPORTER_STALE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_ACTIVE_REPORTER_STALE_SECS),
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

fn read_bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn parse_allowed_server_host_suffixes(raw: &str) -> Vec<String> {
    let mut out = raw
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if out.is_empty() {
        out.push(".wynncraft.com".to_string());
    }
    out
}

#[derive(Default)]
struct Metrics {
    enrolled_total: AtomicU64,
    attest_ok_total: AtomicU64,
    attest_fail_total: AtomicU64,
    signed_replay_reject_total: AtomicU64,
    single_active_reject_total: AtomicU64,
    owner_provisional_total: AtomicU64,
    owner_reverted_total: AtomicU64,
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
    device_pubkey_b64: String,
    device_key_id: String,
    mojang_uuid: String,
    mojang_username: String,
    last_attested_at: DateTime<Utc>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorldAttestation {
    #[serde(default)]
    server_host: String,
    #[serde(default)]
    validity_state: String,
    #[serde(default)]
    observed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packet_hint: Option<String>,
}

#[derive(Clone, Debug)]
struct IdentityRecord {
    reporter_id: String,
    device_pubkey_hash: String,
    device_pubkey_b64: String,
    device_key_id: String,
    mojang_uuid: String,
    mojang_username: String,
    status: String,
    registered_at: DateTime<Utc>,
    last_attested_at: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

#[derive(Clone, Debug)]
struct AttestationChallengeRecord {
    challenge_id: String,
    nonce: String,
    server_id: String,
    device_pubkey_hash: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct ProvisionalOwnershipClaim {
    territory: String,
    claimed_guild_uuid: Option<String>,
    claimed_guild_name: Option<String>,
    claimed_acquired: Option<String>,
    first_seen: Instant,
    expires_at: Instant,
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
    identities: Arc<RwLock<HashMap<String, IdentityRecord>>>,
    challenges: Arc<RwLock<HashMap<String, AttestationChallengeRecord>>>,
    seen_signed_nonces: Arc<RwLock<HashMap<String, Instant>>>,
    provisional_ownership: Arc<RwLock<HashMap<String, ProvisionalOwnershipClaim>>>,
    session_verifier_fail_open_until: Arc<RwLock<Option<Instant>>>,
    forward_queue: Arc<RwLock<VecDeque<ForwardJob>>>,
    metrics: Arc<Metrics>,
}

#[derive(Debug, Deserialize)]
struct AttestChallengeRequest {
    #[serde(default)]
    device_pubkey: String,
    #[serde(default)]
    minecraft_version: String,
    #[serde(default)]
    mod_version: String,
}

#[derive(Debug, Serialize)]
struct AttestChallengeResponse {
    ok: bool,
    challenge_id: String,
    nonce: String,
    server_id: String,
    expires_at: String,
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
    #[serde(default)]
    challenge_id: Option<String>,
    #[serde(default)]
    device_pubkey: Option<String>,
    #[serde(default)]
    device_sig: Option<String>,
    #[serde(default)]
    mojang_uuid: Option<String>,
    #[serde(default)]
    mojang_username: Option<String>,
    #[serde(default)]
    server_id: Option<String>,
    #[serde(default)]
    world_attestation: Option<WorldAttestation>,
    #[serde(default)]
    session_token: Option<String>,
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
    #[serde(default)]
    world_attestation: Option<WorldAttestation>,
    #[serde(default)]
    session_refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReporterTerritoryBatch {
    #[serde(default)]
    generated_at: String,
    #[serde(default)]
    world_attestation: Option<WorldAttestation>,
    #[serde(default)]
    session_refresh_token: Option<String>,
    #[serde(default)]
    updates: Vec<CanonicalTerritoryUpdate>,
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
    device_key_id: String,
    device_pubkey_b64: String,
    mojang_uuid: String,
    mojang_username: String,
    last_attested_at: DateTime<Utc>,
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
        identities: Arc::new(RwLock::new(HashMap::new())),
        challenges: Arc::new(RwLock::new(HashMap::new())),
        seen_signed_nonces: Arc::new(RwLock::new(HashMap::new())),
        provisional_ownership: Arc::new(RwLock::new(HashMap::new())),
        session_verifier_fail_open_until: Arc::new(RwLock::new(None)),
        forward_queue: Arc::new(RwLock::new(VecDeque::new())),
        metrics: Arc::new(Metrics::default()),
    };

    bootstrap_reporters(&state).await?;

    spawn_retention_task(state.clone());
    spawn_forwarder_task(state.clone());
    spawn_ownership_corroborator_task(state.clone());

    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/attest/challenge", post(attest_challenge))
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
# TYPE sequoia_ingest_attest_ok_total counter\nsequoia_ingest_attest_ok_total {}\n\
# TYPE sequoia_ingest_attest_fail_total counter\nsequoia_ingest_attest_fail_total {}\n\
# TYPE sequoia_ingest_signed_replay_reject_total counter\nsequoia_ingest_signed_replay_reject_total {}\n\
# TYPE sequoia_ingest_single_active_reject_total counter\nsequoia_ingest_single_active_reject_total {}\n\
# TYPE sequoia_owner_provisional_total counter\nsequoia_owner_provisional_total {}\n\
# TYPE sequoia_owner_reverted_total counter\nsequoia_owner_reverted_total {}\n\
# TYPE sequoia_ingest_reports_accepted_total counter\nsequoia_ingest_reports_accepted_total {}\n\
# TYPE sequoia_ingest_reports_rejected_total counter\nsequoia_ingest_reports_rejected_total {}\n\
# TYPE sequoia_ingest_reports_degraded_total counter\nsequoia_ingest_reports_degraded_total {}\n\
# TYPE sequoia_ingest_reports_quorum_total counter\nsequoia_ingest_reports_quorum_total {}\n\
# TYPE sequoia_ingest_forward_failures_total counter\nsequoia_ingest_forward_failures_total {}\n",
        state.metrics.enrolled_total.load(Ordering::Relaxed),
        state.metrics.attest_ok_total.load(Ordering::Relaxed),
        state.metrics.attest_fail_total.load(Ordering::Relaxed),
        state
            .metrics
            .signed_replay_reject_total
            .load(Ordering::Relaxed),
        state
            .metrics
            .single_active_reject_total
            .load(Ordering::Relaxed),
        state
            .metrics
            .owner_provisional_total
            .load(Ordering::Relaxed),
        state.metrics.owner_reverted_total.load(Ordering::Relaxed),
        state.metrics.reports_accepted_total.load(Ordering::Relaxed),
        state.metrics.reports_rejected_total.load(Ordering::Relaxed),
        state.metrics.reports_degraded_total.load(Ordering::Relaxed),
        state.metrics.reports_quorum_total.load(Ordering::Relaxed),
        state.metrics.forward_failures_total.load(Ordering::Relaxed),
    )
}

async fn attest_challenge(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AttestChallengeResponse>, StatusCode> {
    let ip = resolve_client_ip(&headers, addr, &state.cfg.trusted_proxy_cidrs).to_string();
    if !check_rate_limit_ip(&state, &ip).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let req: AttestChallengeRequest = parse_json_body(&body)?;
    if decode_ed25519_public_key(&req.device_pubkey).is_none() {
        state
            .metrics
            .attest_fail_total
            .fetch_add(1, Ordering::Relaxed);
        return Err(StatusCode::BAD_REQUEST);
    }

    let now = Utc::now();
    let challenge_id = Uuid::new_v4().to_string();
    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let server_id = format!("iris:{}", Uuid::new_v4().simple());
    let expires_at =
        now + chrono::Duration::seconds(i64::try_from(CHALLENGE_TTL_SECS).unwrap_or(120));
    let device_pubkey_hash = token_hash(req.device_pubkey.trim());

    let challenge = AttestationChallengeRecord {
        challenge_id: challenge_id.clone(),
        nonce: nonce.clone(),
        server_id: server_id.clone(),
        device_pubkey_hash: device_pubkey_hash.clone(),
        issued_at: now,
        expires_at,
        used_at: None,
    };

    {
        let mut challenges = state.challenges.write().await;
        challenges.insert(challenge_id.clone(), challenge.clone());
    }
    persist_attestation_challenge(&state, &challenge).await;

    Ok(Json(AttestChallengeResponse {
        ok: true,
        challenge_id,
        nonce,
        server_id,
        expires_at: expires_at.to_rfc3339(),
    }))
}

async fn enroll(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EnrollResponse>, StatusCode> {
    let ip = resolve_client_ip(&headers, addr, &state.cfg.trusted_proxy_cidrs).to_string();
    if !check_rate_limit_ip(&state, &ip).await {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let req: EnrollRequest = parse_json_body(&body)?;

    let now = Utc::now();
    let guild_opt_in = req.guild_opt_in.unwrap_or(false);
    let field_toggles = req.field_toggles.clone().unwrap_or_default();

    let device_pubkey = req
        .device_pubkey
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let device_pubkey_hash = token_hash(&device_pubkey);
    let mojang_uuid = req
        .mojang_uuid
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let mojang_username = req
        .mojang_username
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let server_id = req.server_id.clone().unwrap_or_default().trim().to_string();
    let challenge_id = req
        .challenge_id
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let session_token = req
        .session_token
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let world_attestation = req.world_attestation.clone();

    if state.cfg.auth_required {
        let challenge = consume_attestation_challenge(
            &state,
            &challenge_id,
            &device_pubkey_hash,
            &server_id,
            now,
        )
        .await?;

        validate_world_attestation(&state, world_attestation.as_ref(), now)?;
        verify_enroll_signature(&req, &challenge.nonce)?;
        if state.cfg.require_session_proof {
            verify_enrollment_session(
                &state,
                &session_token,
                &mojang_uuid,
                &mojang_username,
                &server_id,
            )
            .await?;
        }
        state
            .metrics
            .attest_ok_total
            .fetch_add(1, Ordering::Relaxed);
    }

    let authed_reporter = authenticate(&state, &headers)
        .await
        .ok()
        .map(|authed| authed.reporter_id);

    let mut reporter_id = authed_reporter.unwrap_or_else(|| Uuid::new_v4().to_string());
    if state.cfg.single_reporter_mode {
        if let Some(existing_reporter_id) =
            enforce_single_reporter_mode(&state, &device_pubkey_hash, &mojang_uuid, now).await?
        {
            reporter_id = existing_reporter_id;
        }
    }

    let token = new_token();
    let token_hash_value = token_hash(&token);
    let token_expires_at = now + chrono::TimeDelta::hours(TOKEN_TTL_HOURS);

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

        let key_id = hash_prefix(&device_pubkey_hash, 16);
        let record = ReporterRecord {
            token_hash: token_hash_value.clone(),
            token_expires_at,
            revoked: false,
            guild_opt_in,
            field_toggles: field_toggles.clone(),
            last_seen: now,
            device_pubkey_b64: device_pubkey.clone(),
            device_key_id: key_id.clone(),
            mojang_uuid: mojang_uuid.clone(),
            mojang_username: mojang_username.clone(),
            last_attested_at: now,
        };
        reporters.insert(reporter_id.clone(), record);
        token_index.insert(token_hash_value.clone(), reporter_id.clone());
    }

    persist_reporter(
        &state,
        &reporter_id,
        &token_hash_value,
        token_expires_at,
        guild_opt_in,
        &field_toggles,
        now,
        false,
        &device_pubkey,
        &mojang_uuid,
        &mojang_username,
        now,
    )
    .await;

    persist_identity(
        &state,
        &reporter_id,
        &device_pubkey_hash,
        &device_pubkey,
        &mojang_uuid,
        &mojang_username,
        "active",
        now,
        now,
        now,
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
    body: Bytes,
) -> Result<Json<HeartbeatResponse>, StatusCode> {
    let req: HeartbeatRequest = parse_json_body(&body)?;
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

    if state.cfg.auth_required {
        verify_signed_request(
            &state,
            &headers,
            "POST",
            "/v1/heartbeat",
            &body,
            &authed.reporter_id,
            &authed.device_key_id,
            &authed.device_pubkey_b64,
        )
        .await?;
        validate_world_attestation(&state, req.world_attestation.as_ref(), Utc::now())?;
        maybe_refresh_session_attestation(
            &state,
            &authed,
            req.session_refresh_token.as_deref(),
            Utc::now(),
        )
        .await?;
    }

    let now = Utc::now();
    let (
        token_expires_at,
        guild_opt_in,
        field_toggles,
        rotated_token,
        token_hash_to_persist,
        device_pubkey_b64,
        mojang_uuid,
        mojang_username,
        last_attested_at,
    ) = {
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
            record.device_pubkey_b64.clone(),
            record.mojang_uuid.clone(),
            record.mojang_username.clone(),
            record.last_attested_at,
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
        &device_pubkey_b64,
        &mojang_uuid,
        &mojang_username,
        last_attested_at,
    )
    .await;

    touch_identity_last_seen(&state, &authed.reporter_id, now).await;

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
    body: Bytes,
) -> Result<Json<ReportAck>, StatusCode> {
    let mut batch: ReporterTerritoryBatch = parse_json_body(&body)?;
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

    if state.cfg.auth_required {
        verify_signed_request(
            &state,
            &headers,
            "POST",
            "/v1/report/territory",
            &body,
            &authed.reporter_id,
            &authed.device_key_id,
            &authed.device_pubkey_b64,
        )
        .await?;
        validate_world_attestation(&state, batch.world_attestation.as_ref(), Utc::now())?;
        maybe_refresh_session_attestation(
            &state,
            &authed,
            batch.session_refresh_token.as_deref(),
            Utc::now(),
        )
        .await?;
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

        let ownership_claim = update.guild.clone().map(|guild| (guild.uuid, guild.name));
        let acquired_claim = update.acquired.clone();

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

            if state.cfg.owner_soft_corroboration
                && ownership_claim.is_some()
                && !was_quorum
                && state.cfg.owner_revert_on_mismatch
            {
                if let Some(runtime) = accepted_update.runtime.as_mut() {
                    let mut provenance = runtime
                        .provenance
                        .clone()
                        .unwrap_or_else(default_provenance);
                    provenance.confidence = provenance.confidence.min(0.55).max(0.35);
                    runtime.provenance = Some(provenance);
                }
                let (guild_uuid, guild_name) = ownership_claim.clone().unwrap_or_default();
                register_provisional_ownership(
                    &state,
                    &accepted_update.territory,
                    if guild_uuid.is_empty() {
                        None
                    } else {
                        Some(guild_uuid)
                    },
                    if guild_name.is_empty() {
                        None
                    } else {
                        Some(guild_name)
                    },
                    acquired_claim.clone(),
                )
                .await;
                state
                    .metrics
                    .owner_provisional_total
                    .fetch_add(1, Ordering::Relaxed);
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

    touch_identity_last_seen(&state, &authed.reporter_id, Utc::now()).await;

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
    let (
        revoked,
        expired,
        field_toggles,
        device_key_id,
        device_pubkey_b64,
        mojang_uuid,
        mojang_username,
        last_attested_at,
    ) = {
        let reporters = state.reporters.read().await;
        let Some(record) = reporters.get(&reporter_id) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        (
            record.revoked,
            record.token_expires_at <= now,
            record.field_toggles.clone(),
            record.device_key_id.clone(),
            record.device_pubkey_b64.clone(),
            record.mojang_uuid.clone(),
            record.mojang_username.clone(),
            record.last_attested_at,
        )
    };

    if revoked || expired {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(AuthedReporter {
        reporter_id,
        field_toggles,
        device_key_id,
        device_pubkey_b64,
        mojang_uuid,
        mojang_username,
        last_attested_at,
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

fn parse_json_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, StatusCode> {
    serde_json::from_slice::<T>(body).map_err(|_| StatusCode::BAD_REQUEST)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn hash_prefix(value: &str, max: usize) -> String {
    value.chars().take(max).collect::<String>()
}

fn decode_base64_bytes(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(value.trim())
                .ok()
        })
}

fn decode_ed25519_public_key(value: &str) -> Option<VerifyingKey> {
    let bytes = decode_base64_bytes(value)?;
    if bytes.len() == 32 {
        let array: [u8; 32] = bytes.try_into().ok()?;
        return VerifyingKey::from_bytes(&array).ok();
    }
    VerifyingKey::from_public_key_der(&bytes).ok()
}

fn verify_ed25519_signature(pubkey_b64: &str, message: &[u8], sig_b64: &str) -> bool {
    let Some(pubkey) = decode_ed25519_public_key(pubkey_b64) else {
        return false;
    };
    let Some(sig_bytes) = decode_base64_bytes(sig_b64) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_array);
    pubkey.verify(message, &signature).is_ok()
}

fn signed_payload_hash_hex(body: &Bytes) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_ref());
    hex::encode(hasher.finalize())
}

fn build_signed_message(
    method: &str,
    path: &str,
    ts: &str,
    nonce: &str,
    body_hash_hex: &str,
    reporter_id: &str,
) -> String {
    format!("{method}\n{path}\n{ts}\n{nonce}\n{body_hash_hex}\n{reporter_id}")
}

async fn verify_signed_request(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &Bytes,
    reporter_id: &str,
    expected_key_id: &str,
    expected_pubkey_b64: &str,
) -> Result<(), StatusCode> {
    if !state.cfg.auth_required {
        return Ok(());
    }

    let key_id = header_str(headers, HDR_IRIS_KEY_ID).ok_or(StatusCode::UNAUTHORIZED)?;
    if key_id.trim() != expected_key_id {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let ts_raw = header_str(headers, HDR_IRIS_TS).ok_or(StatusCode::UNAUTHORIZED)?;
    let ts = ts_raw
        .trim()
        .parse::<i64>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let now_ts = Utc::now().timestamp();
    let skew = (now_ts - ts).abs();
    if skew > i64::try_from(state.cfg.signed_nonce_window_secs).unwrap_or(300) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let nonce = header_str(headers, HDR_IRIS_NONCE).ok_or(StatusCode::UNAUTHORIZED)?;
    if nonce.trim().is_empty() || nonce.len() > 256 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let nonce_claim_key = format!("{reporter_id}:{}", nonce.trim());
    if !claim_signed_nonce(
        state,
        &nonce_claim_key,
        Duration::from_secs(state.cfg.signed_nonce_window_secs),
    )
    .await
    {
        state
            .metrics
            .signed_replay_reject_total
            .fetch_add(1, Ordering::Relaxed);
        return Err(StatusCode::UNAUTHORIZED);
    }

    let sig = header_str(headers, HDR_IRIS_SIG).ok_or(StatusCode::UNAUTHORIZED)?;
    let body_hash = signed_payload_hash_hex(body);
    let message = build_signed_message(
        method,
        path,
        ts_raw.trim(),
        nonce.trim(),
        &body_hash,
        reporter_id,
    );
    if !verify_ed25519_signature(expected_pubkey_b64, message.as_bytes(), sig.trim()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(())
}

async fn claim_signed_nonce(state: &Arc<AppState>, key: &str, ttl: Duration) -> bool {
    let now = Instant::now();
    let mut seen = state.seen_signed_nonces.write().await;
    seen.retain(|_, expires| *expires > now);

    if !seen.contains_key(key)
        && state.cfg.max_signed_nonce_keys > 0
        && seen.len() >= state.cfg.max_signed_nonce_keys
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

fn host_suffix_allowed(host: &str, allowed_suffixes: &[String]) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    allowed_suffixes
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

fn validate_world_attestation(
    state: &Arc<AppState>,
    attestation: Option<&WorldAttestation>,
    now: DateTime<Utc>,
) -> Result<(), StatusCode> {
    if !state.cfg.auth_required {
        return Ok(());
    }
    let Some(attestation) = attestation else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !host_suffix_allowed(
        &attestation.server_host,
        &state.cfg.allowed_server_host_suffixes,
    ) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if attestation.validity_state.trim() != "valid" {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let observed_at = DateTime::parse_from_rfc3339(attestation.observed_at.trim())
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let age = now.signed_duration_since(observed_at).num_seconds();
    if age < 0 || age > i64::try_from(state.cfg.world_attestation_max_age_secs).unwrap_or(120) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn consume_attestation_challenge(
    state: &Arc<AppState>,
    challenge_id: &str,
    device_pubkey_hash: &str,
    server_id: &str,
    now: DateTime<Utc>,
) -> Result<AttestationChallengeRecord, StatusCode> {
    let mut challenges = state.challenges.write().await;
    let Some(challenge) = challenges.get_mut(challenge_id).cloned() else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if challenge.device_pubkey_hash != device_pubkey_hash
        || challenge.server_id != server_id
        || challenge.expires_at <= now
        || challenge.used_at.is_some()
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let mut updated = challenge.clone();
    updated.used_at = Some(now);
    challenges.insert(challenge_id.to_string(), updated.clone());
    persist_challenge_used_at(state, challenge_id, now).await;
    Ok(updated)
}

fn verify_enroll_signature(req: &EnrollRequest, challenge_nonce: &str) -> Result<(), StatusCode> {
    let device_pubkey = req
        .device_pubkey
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let sig = req
        .device_sig
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let challenge_id = req
        .challenge_id
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let mojang_uuid = req
        .mojang_uuid
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let mojang_username = req
        .mojang_username
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let server_id = req.server_id.as_deref().map(str::trim).unwrap_or_default();
    let observed_at = req
        .world_attestation
        .as_ref()
        .map(|att| att.observed_at.trim())
        .unwrap_or_default();
    let device_pubkey_hash = token_hash(device_pubkey);
    let message = format!(
        "enroll\n{challenge_id}\n{challenge_nonce}\n{server_id}\n{mojang_uuid}\n{mojang_username}\n{device_pubkey_hash}\n{observed_at}"
    );
    if !verify_ed25519_signature(device_pubkey, message.as_bytes(), sig) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn verify_enrollment_session(
    state: &Arc<AppState>,
    session_token: &str,
    mojang_uuid: &str,
    mojang_username: &str,
    _server_id: &str,
) -> Result<(), StatusCode> {
    if session_token.trim().is_empty()
        || mojang_uuid.trim().is_empty()
        || mojang_username.trim().is_empty()
    {
        state
            .metrics
            .attest_fail_total
            .fetch_add(1, Ordering::Relaxed);
        return Err(StatusCode::UNAUTHORIZED);
    }
    match verify_minecraft_session_token(state, session_token, mojang_uuid, mojang_username).await {
        SessionVerifyResult::Verified => {
            let mut fail_open = state.session_verifier_fail_open_until.write().await;
            *fail_open = None;
            Ok(())
        }
        SessionVerifyResult::Invalid => {
            state
                .metrics
                .attest_fail_total
                .fetch_add(1, Ordering::Relaxed);
            Err(StatusCode::UNAUTHORIZED)
        }
        SessionVerifyResult::Unavailable => {
            state
                .metrics
                .attest_fail_total
                .fetch_add(1, Ordering::Relaxed);
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn maybe_refresh_session_attestation(
    state: &Arc<AppState>,
    authed: &AuthedReporter,
    session_refresh_token: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), StatusCode> {
    if !state.cfg.require_session_proof {
        return Ok(());
    }
    let refresh_after = authed.last_attested_at
        + chrono::Duration::seconds(
            i64::try_from(state.cfg.session_refresh_interval_secs).unwrap_or(600),
        );
    if now < refresh_after {
        return Ok(());
    }

    let Some(token) = session_refresh_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        let grace_until = authed.last_attested_at
            + chrono::Duration::seconds(
                i64::try_from(state.cfg.session_fail_open_grace_secs).unwrap_or(1800),
            );
        if now <= grace_until {
            return Ok(());
        }
        return Err(StatusCode::UNAUTHORIZED);
    };

    match verify_minecraft_session_token(state, token, &authed.mojang_uuid, &authed.mojang_username)
        .await
    {
        SessionVerifyResult::Verified => {
            let mut fail_open = state.session_verifier_fail_open_until.write().await;
            *fail_open = None;
            drop(fail_open);
            update_reporter_last_attested(state, &authed.reporter_id, now).await;
            Ok(())
        }
        SessionVerifyResult::Invalid => {
            let mut fail_open = state.session_verifier_fail_open_until.write().await;
            *fail_open = None;
            Err(StatusCode::UNAUTHORIZED)
        }
        SessionVerifyResult::Unavailable => {
            let mut fail_open = state.session_verifier_fail_open_until.write().await;
            if session_verifier_within_fail_open_grace(
                &mut fail_open,
                Instant::now(),
                state.cfg.session_fail_open_grace_secs,
            ) {
                Ok(())
            } else {
                Err(StatusCode::SERVICE_UNAVAILABLE)
            }
        }
    }
}

fn session_verifier_within_fail_open_grace(
    fail_open_until: &mut Option<Instant>,
    now: Instant,
    grace_secs: u64,
) -> bool {
    let deadline = if let Some(existing_deadline) = *fail_open_until {
        existing_deadline
    } else {
        let created_deadline = now + Duration::from_secs(grace_secs);
        *fail_open_until = Some(created_deadline);
        created_deadline
    };
    now <= deadline
}

enum SessionVerifyResult {
    Verified,
    Invalid,
    Unavailable,
}

async fn verify_minecraft_session_token(
    state: &Arc<AppState>,
    session_token: &str,
    expected_uuid: &str,
    expected_username: &str,
) -> SessionVerifyResult {
    let request = state
        .http
        .get("https://api.minecraftservices.com/minecraft/profile")
        .header("authorization", format!("Bearer {}", session_token.trim()));
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return SessionVerifyResult::Unavailable,
    };

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return SessionVerifyResult::Invalid;
    }
    if !response.status().is_success() {
        return SessionVerifyResult::Unavailable;
    }

    let json = match response.json::<serde_json::Value>().await {
        Ok(value) => value,
        Err(_) => return SessionVerifyResult::Unavailable,
    };

    let actual_uuid = json
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual_name = json
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected_uuid_normalized = expected_uuid.trim().replace('-', "").to_ascii_lowercase();
    let expected_name_normalized = expected_username.trim().to_ascii_lowercase();

    if actual_uuid.is_empty()
        || actual_name.is_empty()
        || actual_uuid != expected_uuid_normalized
        || actual_name != expected_name_normalized
    {
        return SessionVerifyResult::Invalid;
    }

    SessionVerifyResult::Verified
}

async fn enforce_single_reporter_mode(
    state: &Arc<AppState>,
    device_pubkey_hash: &str,
    mojang_uuid: &str,
    now: DateTime<Utc>,
) -> Result<Option<String>, StatusCode> {
    if !state.cfg.single_reporter_mode {
        return Ok(None);
    }

    let identities = state.identities.read().await;
    let active_identity = identities
        .values()
        .filter(|identity| identity.status == "active")
        .max_by_key(|identity| identity.last_seen);
    let Some(active_identity) = active_identity else {
        return Ok(None);
    };

    if active_identity.device_pubkey_hash == device_pubkey_hash {
        return Ok(Some(active_identity.reporter_id.clone()));
    }

    let stale_before = now
        - chrono::Duration::seconds(
            i64::try_from(state.cfg.active_reporter_stale_secs).unwrap_or(1800),
        );
    let stale_enough = active_identity.last_seen <= stale_before;
    let same_account = !mojang_uuid.trim().is_empty() && active_identity.mojang_uuid == mojang_uuid;
    if stale_enough && same_account {
        return Ok(Some(active_identity.reporter_id.clone()));
    }

    state
        .metrics
        .single_active_reject_total
        .fetch_add(1, Ordering::Relaxed);
    Err(StatusCode::FORBIDDEN)
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
    if limit == 0 {
        // Explicit zero disables this limiter without rejecting traffic.
        return true;
    }

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
        let mut accepted = update.clone();
        if let Some(runtime) = bucket
            .iter()
            .rev()
            .find(|claim| claim.claim_hash == claim_hash)
            .map(|claim| claim.update.runtime.clone())
            .flatten()
        {
            accepted.runtime = Some(runtime);
        }

        let mut runtime = accepted.runtime.take().unwrap_or_default();
        let mut provenance = runtime.provenance.take().unwrap_or_else(default_provenance);
        provenance.reporter_count = corroborating as u16;
        runtime.provenance = Some(provenance);
        accepted.runtime = Some(runtime);

        bucket.retain(|claim| claim.claim_hash != claim_hash);
        return Some((accepted, degraded_ok, quorum_ok));
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

async fn register_provisional_ownership(
    state: &Arc<AppState>,
    territory: &str,
    claimed_guild_uuid: Option<String>,
    claimed_guild_name: Option<String>,
    claimed_acquired: Option<String>,
) {
    let now = Instant::now();
    let expires_at = now + Duration::from_secs(state.cfg.owner_corroboration_window_secs);
    let mut claims = state.provisional_ownership.write().await;
    claims.insert(
        territory.to_string(),
        ProvisionalOwnershipClaim {
            territory: territory.to_string(),
            claimed_guild_uuid,
            claimed_guild_name,
            claimed_acquired,
            first_seen: now,
            expires_at,
        },
    );
}

fn spawn_ownership_corroborator_task(state: AppState) {
    tokio::spawn(async move {
        let state = Arc::new(state);
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if !state.cfg.owner_soft_corroboration || !state.cfg.owner_revert_on_mismatch {
                continue;
            }
            let now = Instant::now();
            let due_claims = {
                let claims = state.provisional_ownership.read().await;
                claims
                    .values()
                    .filter(|claim| claim.expires_at <= now)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if due_claims.is_empty() {
                continue;
            }

            let authoritative = match fetch_wynncraft_ownership_map(&state).await {
                Ok(map) => map,
                Err(err) => {
                    warn!(error = %err, "failed to fetch authoritative territory map for corroboration");
                    continue;
                }
            };

            let mut remove_keys = Vec::new();
            for claim in due_claims {
                let Some(authoritative_entry) = authoritative.get(&claim.territory) else {
                    remove_keys.push(claim.territory.clone());
                    continue;
                };
                let mismatch = claim
                    .claimed_guild_uuid
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    != authoritative_entry
                        .0
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default();
                if !mismatch {
                    remove_keys.push(claim.territory.clone());
                    continue;
                }

                let correction = CanonicalTerritoryUpdate {
                    territory: claim.territory.clone(),
                    guild: authoritative_entry.1.clone(),
                    acquired: authoritative_entry.2.clone(),
                    location: None,
                    resources: None,
                    connections: None,
                    runtime: Some(TerritoryRuntimeData {
                        provenance: Some(DataProvenance {
                            source: "wynncraft_authoritative_revert".to_string(),
                            visibility: VisibilityClass::Public,
                            confidence: 1.0,
                            reporter_count: 0,
                            observed_at: Utc::now().to_rfc3339(),
                            menu_season_id: None,
                            menu_captured_territories: None,
                            menu_sr_per_hour: None,
                            menu_observed_at: None,
                        }),
                        ..TerritoryRuntimeData::default()
                    }),
                    idempotency_key: Some(format!(
                        "owner-revert:{}:{}",
                        claim.territory,
                        Uuid::new_v4().simple()
                    )),
                };
                enqueue_forward(
                    &state,
                    "/api/internal/ingest/territory",
                    serde_json::to_value(CanonicalTerritoryBatch {
                        generated_at: Utc::now().to_rfc3339(),
                        updates: vec![correction],
                    })
                    .unwrap_or_else(
                        |_| serde_json::json!({"generated_at": Utc::now().to_rfc3339(), "updates": []}),
                    ),
                )
                .await;
                state
                    .metrics
                    .owner_reverted_total
                    .fetch_add(1, Ordering::Relaxed);
                remove_keys.push(claim.territory.clone());
            }

            if !remove_keys.is_empty() {
                let mut claims = state.provisional_ownership.write().await;
                for key in remove_keys {
                    claims.remove(&key);
                }
            }
        }
    });
}

async fn fetch_wynncraft_ownership_map(
    state: &Arc<AppState>,
) -> Result<
    HashMap<
        String,
        (
            Option<String>,
            Option<sequoia_shared::GuildRef>,
            Option<String>,
        ),
    >,
    String,
> {
    let response = state
        .http
        .get("https://api.wynncraft.com/v3/guild/list/territory")
        .send()
        .await
        .map_err(|err| format!("request authoritative territory map: {err}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "authoritative territory map returned status {}",
            response.status()
        ));
    }
    let body = response
        .json::<serde_json::Value>()
        .await
        .map_err(|err| format!("decode authoritative territory map: {err}"))?;

    let mut out = HashMap::new();
    let Some(entries) = body.as_object() else {
        return Err("authoritative territory payload is not an object".to_string());
    };
    for (territory, value) in entries {
        let guild = value.get("guild");
        let guild_uuid = guild
            .and_then(|v| v.get("uuid"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        let guild_name = guild
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        let guild_prefix = guild
            .and_then(|v| v.get("prefix"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .unwrap_or_default();
        let guild_ref = match (guild_uuid.clone(), guild_name.clone()) {
            (Some(uuid), Some(name)) if !uuid.trim().is_empty() && !name.trim().is_empty() => {
                Some(sequoia_shared::GuildRef {
                    uuid,
                    name,
                    prefix: guild_prefix,
                    color: None,
                })
            }
            _ => None,
        };
        let acquired = value
            .get("acquired")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        out.insert(territory.to_string(), (guild_uuid, guild_ref, acquired));
    }
    Ok(out)
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

    let now_utc = Utc::now();
    let now_rfc3339 = now_utc.to_rfc3339();
    sqlx::query("DELETE FROM attestation_challenges WHERE expires_at < ? OR used_at IS NOT NULL")
        .bind(&now_rfc3339)
        .execute(&state.db)
        .await
        .map_err(|e| format!("delete expired/used attestation_challenges: {e}"))?;

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
        let mut seen = state.seen_signed_nonces.write().await;
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
        let mut claims = state.provisional_ownership.write().await;
        claims.retain(|_, claim| claim.expires_at > now);
    }
    {
        let mut challenges = state.challenges.write().await;
        challenges.retain(|_, challenge| challenge.expires_at > now_utc);
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
         device_pubkey_b64 TEXT NOT NULL DEFAULT '',\
         device_key_id TEXT NOT NULL DEFAULT '',\
         mojang_uuid TEXT NOT NULL DEFAULT '',\
         mojang_username TEXT NOT NULL DEFAULT '',\
         last_attested_at TEXT NOT NULL DEFAULT (datetime('now')),\
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
    sqlx::query("ALTER TABLE reporters ADD COLUMN device_pubkey_b64 TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN device_key_id TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN mojang_uuid TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await
        .ok();
    sqlx::query("ALTER TABLE reporters ADD COLUMN mojang_username TEXT NOT NULL DEFAULT ''")
        .execute(pool)
        .await
        .ok();
    sqlx::query(
        "ALTER TABLE reporters ADD COLUMN last_attested_at TEXT NOT NULL DEFAULT (datetime('now'))",
    )
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

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS reporter_identities (\
         reporter_id TEXT PRIMARY KEY,\
         device_pubkey_hash TEXT NOT NULL,\
         device_pubkey_b64 TEXT NOT NULL,\
         device_key_id TEXT NOT NULL,\
         mojang_uuid TEXT NOT NULL,\
         mojang_username TEXT NOT NULL,\
         status TEXT NOT NULL DEFAULT 'active',\
         registered_at TEXT NOT NULL,\
         last_attested_at TEXT NOT NULL,\
         last_seen TEXT NOT NULL\
         )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS attestation_challenges (\
         challenge_id TEXT PRIMARY KEY,\
         nonce TEXT NOT NULL,\
         server_id TEXT NOT NULL,\
         device_pubkey_hash TEXT NOT NULL,\
         issued_at TEXT NOT NULL,\
         expires_at TEXT NOT NULL,\
         used_at TEXT\
         )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_attestation_challenges_expires_at \
         ON attestation_challenges (expires_at)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_attestation_challenges_used_at \
         ON attestation_challenges (used_at)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn bootstrap_reporters(state: &AppState) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT reporter_id, token, token_expires_at, guild_opt_in, \
                share_owner, share_headquarters, share_held_resources, \
                share_production_rates, share_storage_capacity, share_defense_tier, share_trading_routes, \
                device_pubkey_b64, device_key_id, mojang_uuid, mojang_username, revoked, last_attested_at, last_seen \
         FROM reporters",
    )
    .fetch_all(&state.db)
    .await?;

    let mut reporters = state.reporters.write().await;
    let mut token_index = state.token_index.write().await;
    let mut bootstrapped_identities: Vec<IdentityRecord> = Vec::new();
    let mut migrated_plaintext_tokens = 0_usize;

    for row in rows {
        let reporter_id: String = row.get("reporter_id");
        let token: String = row.get("token");
        let token_expires_at: String = row.get("token_expires_at");
        let guild_opt_in: i64 = row.get("guild_opt_in");
        let share_owner: i64 = row.get("share_owner");
        let share_headquarters: i64 = row.get("share_headquarters");
        let share_held_resources: i64 = row.get("share_held_resources");
        let share_production_rates: i64 = row.get("share_production_rates");
        let share_storage_capacity: i64 = row.get("share_storage_capacity");
        let share_defense_tier: i64 = row.get("share_defense_tier");
        let share_trading_routes: i64 = row.get("share_trading_routes");
        let device_pubkey_b64: String = row.get("device_pubkey_b64");
        let device_key_id: String = row.get("device_key_id");
        let mojang_uuid: String = row.get("mojang_uuid");
        let mojang_username: String = row.get("mojang_username");
        let revoked: i64 = row.get("revoked");
        let last_attested_at: String = row.get("last_attested_at");
        let last_seen: String = row.get("last_seen");

        let persisted_token_hash = normalize_persisted_token(&token);
        let token_expires_at = DateTime::parse_from_rfc3339(&token_expires_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() + chrono::TimeDelta::hours(TOKEN_TTL_HOURS));
        let last_seen = DateTime::parse_from_rfc3339(&last_seen)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let last_attested_at = DateTime::parse_from_rfc3339(&last_attested_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(last_seen);
        let key_id = if device_key_id.trim().is_empty() {
            hash_prefix(&token_hash(&device_pubkey_b64), 16)
        } else {
            device_key_id.clone()
        };
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
            device_pubkey_b64: device_pubkey_b64.clone(),
            device_key_id: key_id.clone(),
            mojang_uuid: mojang_uuid.clone(),
            mojang_username: mojang_username.clone(),
            last_attested_at,
        };
        token_index.insert(persisted_token_hash.clone(), reporter_id.clone());
        reporters.insert(reporter_id.clone(), record);

        bootstrapped_identities.push(IdentityRecord {
            reporter_id: reporter_id.clone(),
            device_pubkey_hash: token_hash(&device_pubkey_b64),
            device_pubkey_b64,
            device_key_id: key_id,
            mojang_uuid,
            mojang_username,
            status: if revoked != 0 {
                "revoked".to_string()
            } else {
                "active".to_string()
            },
            registered_at: last_seen,
            last_attested_at,
            last_seen,
        });

        if persisted_token_hash != token {
            migrated_plaintext_tokens += 1;
        }
    }

    drop(token_index);
    drop(reporters);

    {
        let mut identities = state.identities.write().await;
        for identity in bootstrapped_identities {
            identities.insert(identity.reporter_id.clone(), identity);
        }
    }

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

    bootstrap_identities(state).await?;
    bootstrap_challenges(state).await?;
    Ok(())
}

async fn persist_reporter(
    state: &AppState,
    reporter_id: &str,
    token_hash_value: &str,
    token_expires_at: DateTime<Utc>,
    guild_opt_in: bool,
    field_toggles: &ReporterFieldToggles,
    last_seen: DateTime<Utc>,
    revoked: bool,
    device_pubkey_b64: &str,
    mojang_uuid: &str,
    mojang_username: &str,
    last_attested_at: DateTime<Utc>,
) {
    if let Err(err) = sqlx::query(
        "INSERT INTO reporters (reporter_id, token, token_expires_at, guild_opt_in, \
                               share_owner, share_headquarters, share_held_resources, \
                               share_production_rates, share_storage_capacity, share_defense_tier, share_trading_routes, \
                               device_pubkey_b64, device_key_id, mojang_uuid, mojang_username, last_attested_at, \
                               revoked, last_seen) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
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
           device_pubkey_b64=excluded.device_pubkey_b64, \
           device_key_id=excluded.device_key_id, \
           mojang_uuid=excluded.mojang_uuid, \
           mojang_username=excluded.mojang_username, \
           last_attested_at=excluded.last_attested_at, \
           revoked=excluded.revoked, \
           last_seen=excluded.last_seen",
    )
    .bind(reporter_id)
    .bind(token_hash_value)
    .bind(token_expires_at.to_rfc3339())
    .bind(i64::from(guild_opt_in))
    .bind(i64::from(field_toggles.share_owner))
    .bind(i64::from(field_toggles.share_headquarters))
    .bind(i64::from(field_toggles.share_held_resources))
    .bind(i64::from(field_toggles.share_production_rates))
    .bind(i64::from(field_toggles.share_storage_capacity))
    .bind(i64::from(field_toggles.share_defense_tier))
    .bind(i64::from(field_toggles.share_trading_routes))
    .bind(device_pubkey_b64)
    .bind(hash_prefix(&token_hash(device_pubkey_b64), 16))
    .bind(mojang_uuid)
    .bind(mojang_username)
    .bind(last_attested_at.to_rfc3339())
    .bind(i64::from(revoked))
    .bind(last_seen.to_rfc3339())
    .execute(&state.db)
    .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to persist reporter");
    }
}

async fn persist_identity(
    state: &Arc<AppState>,
    reporter_id: &str,
    device_pubkey_hash: &str,
    device_pubkey_b64: &str,
    mojang_uuid: &str,
    mojang_username: &str,
    status: &str,
    registered_at: DateTime<Utc>,
    last_attested_at: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) {
    let key_id = hash_prefix(device_pubkey_hash, 16);
    if let Err(err) = sqlx::query(
        "INSERT INTO reporter_identities (reporter_id, device_pubkey_hash, device_pubkey_b64, device_key_id, mojang_uuid, mojang_username, status, registered_at, last_attested_at, last_seen) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(reporter_id) DO UPDATE SET \
           device_pubkey_hash=excluded.device_pubkey_hash, \
           device_pubkey_b64=excluded.device_pubkey_b64, \
           device_key_id=excluded.device_key_id, \
           mojang_uuid=excluded.mojang_uuid, \
           mojang_username=excluded.mojang_username, \
           status=excluded.status, \
           last_attested_at=excluded.last_attested_at, \
           last_seen=excluded.last_seen",
    )
    .bind(reporter_id)
    .bind(device_pubkey_hash)
    .bind(device_pubkey_b64)
    .bind(&key_id)
    .bind(mojang_uuid)
    .bind(mojang_username)
    .bind(status)
    .bind(registered_at.to_rfc3339())
    .bind(last_attested_at.to_rfc3339())
    .bind(last_seen.to_rfc3339())
    .execute(&state.db)
    .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to persist reporter identity");
    }

    let mut identities = state.identities.write().await;
    identities.insert(
        reporter_id.to_string(),
        IdentityRecord {
            reporter_id: reporter_id.to_string(),
            device_pubkey_hash: device_pubkey_hash.to_string(),
            device_pubkey_b64: device_pubkey_b64.to_string(),
            device_key_id: key_id,
            mojang_uuid: mojang_uuid.to_string(),
            mojang_username: mojang_username.to_string(),
            status: status.to_string(),
            registered_at,
            last_attested_at,
            last_seen,
        },
    );
}

async fn touch_identity_last_seen(
    state: &Arc<AppState>,
    reporter_id: &str,
    last_seen: DateTime<Utc>,
) {
    if let Err(err) =
        sqlx::query("UPDATE reporter_identities SET last_seen = ? WHERE reporter_id = ?")
            .bind(last_seen.to_rfc3339())
            .bind(reporter_id)
            .execute(&state.db)
            .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to update identity last_seen");
    }
    let mut identities = state.identities.write().await;
    if let Some(identity) = identities.get_mut(reporter_id) {
        identity.last_seen = last_seen;
    }
}

async fn update_reporter_last_attested(
    state: &Arc<AppState>,
    reporter_id: &str,
    last_attested_at: DateTime<Utc>,
) {
    let mut exists = false;
    {
        let mut reporters = state.reporters.write().await;
        if let Some(record) = reporters.get_mut(reporter_id) {
            record.last_attested_at = last_attested_at;
            exists = true;
        }
    }
    if exists
        && let Err(err) =
            sqlx::query("UPDATE reporters SET last_attested_at = ? WHERE reporter_id = ?")
                .bind(last_attested_at.to_rfc3339())
                .bind(reporter_id)
                .execute(&state.db)
                .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to update reporter last_attested_at");
    }
    if let Err(err) = sqlx::query(
        "UPDATE reporter_identities SET last_attested_at = ?, last_seen = ? WHERE reporter_id = ?",
    )
    .bind(last_attested_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .bind(reporter_id)
    .execute(&state.db)
    .await
    {
        warn!(reporter_id = %reporter_id, error = %err, "failed to persist identity attestation timestamp");
    }
    let mut identities = state.identities.write().await;
    if let Some(identity) = identities.get_mut(reporter_id) {
        identity.last_attested_at = last_attested_at;
        identity.last_seen = Utc::now();
    }
}

async fn persist_attestation_challenge(
    state: &Arc<AppState>,
    challenge: &AttestationChallengeRecord,
) {
    if let Err(err) = sqlx::query(
        "INSERT OR REPLACE INTO attestation_challenges (challenge_id, nonce, server_id, device_pubkey_hash, issued_at, expires_at, used_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&challenge.challenge_id)
    .bind(&challenge.nonce)
    .bind(&challenge.server_id)
    .bind(&challenge.device_pubkey_hash)
    .bind(challenge.issued_at.to_rfc3339())
    .bind(challenge.expires_at.to_rfc3339())
    .bind(challenge.used_at.map(|value| value.to_rfc3339()))
    .execute(&state.db)
    .await
    {
        warn!(error = %err, challenge_id = %challenge.challenge_id, "failed to persist attestation challenge");
    }
}

async fn persist_challenge_used_at(
    state: &Arc<AppState>,
    challenge_id: &str,
    used_at: DateTime<Utc>,
) {
    if let Err(err) =
        sqlx::query("UPDATE attestation_challenges SET used_at = ? WHERE challenge_id = ?")
            .bind(used_at.to_rfc3339())
            .bind(challenge_id)
            .execute(&state.db)
            .await
    {
        warn!(error = %err, challenge_id = %challenge_id, "failed to update challenge used_at");
    }
}

async fn bootstrap_identities(state: &AppState) -> Result<(), sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, String, String, String, String, String)>(
        "SELECT reporter_id, device_pubkey_hash, device_pubkey_b64, device_key_id, mojang_uuid, mojang_username, status, registered_at, last_attested_at, last_seen FROM reporter_identities",
    )
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }
    let mut identities = state.identities.write().await;
    for (
        reporter_id,
        device_pubkey_hash,
        device_pubkey_b64,
        device_key_id,
        mojang_uuid,
        mojang_username,
        status,
        registered_at,
        last_attested_at,
        last_seen,
    ) in rows
    {
        let registered_at = DateTime::parse_from_rfc3339(&registered_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let last_attested_at = DateTime::parse_from_rfc3339(&last_attested_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(registered_at);
        let last_seen = DateTime::parse_from_rfc3339(&last_seen)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(last_attested_at);
        identities.insert(
            reporter_id.clone(),
            IdentityRecord {
                reporter_id,
                device_pubkey_hash,
                device_pubkey_b64,
                device_key_id,
                mojang_uuid,
                mojang_username,
                status,
                registered_at,
                last_attested_at,
                last_seen,
            },
        );
    }
    Ok(())
}

async fn bootstrap_challenges(state: &AppState) -> Result<(), sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, String, Option<String>)>(
        "SELECT challenge_id, nonce, server_id, device_pubkey_hash, issued_at, expires_at, used_at FROM attestation_challenges",
    )
    .fetch_all(&state.db)
    .await?;
    let now = Utc::now();
    let mut challenges = state.challenges.write().await;
    for (challenge_id, nonce, server_id, device_pubkey_hash, issued_at, expires_at, used_at) in rows
    {
        let issued_at = DateTime::parse_from_rfc3339(&issued_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(now);
        let expires_at = DateTime::parse_from_rfc3339(&expires_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(now);
        if expires_at <= now {
            continue;
        }
        let used_at = used_at
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc));
        challenges.insert(
            challenge_id.clone(),
            AttestationChallengeRecord {
                challenge_id,
                nonce,
                server_id,
                device_pubkey_hash,
                issued_at,
                expires_at,
                used_at,
            },
        );
    }
    Ok(())
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
        AppState, Config, Metrics, ReporterFieldToggles, ReporterRecord, apply_toggle_policy,
        check_rate_limit, evaluate_territory_claim, normalize_idempotency_key,
        normalize_persisted_token, normalize_territory_name, parse_trusted_proxy_cidrs,
        quorum_satisfied, resolve_client_ip, session_verifier_within_fail_open_grace,
        territory_claim_hash, territory_idempotency_hash,
    };
    use axum::http::{HeaderMap, HeaderValue};
    use chrono::Utc;
    use reqwest::Client;
    use sequoia_shared::{
        CanonicalTerritoryUpdate, DataProvenance, GuildRef, TerritoryRuntimeData,
    };
    use sqlx_sqlite::SqlitePoolOptions;
    use std::collections::{HashMap, VecDeque};
    use std::net::{IpAddr, SocketAddr};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::RwLock;

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

    async fn test_state_with_active_reporters(
        degraded_single_reporter_enabled: bool,
        active_reporters: usize,
    ) -> Arc<AppState> {
        let now = Utc::now();
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_lazy("sqlite::memory:")
            .expect("create in-memory sqlite pool");

        let state = Arc::new(AppState {
            cfg: Config {
                bind_addr: "127.0.0.1:3010".to_string(),
                db_url: "sqlite::memory:".to_string(),
                sequoia_server_base_url: "http://127.0.0.1:3000".to_string(),
                internal_ingest_token: "abcdefghijklmnopqrstuvwxyz".to_string(),
                api_body_limit_bytes: 2 * 1024 * 1024,
                max_reporters: 10_000,
                rate_limit_ip_per_min: 300,
                rate_limit_reporter_per_min: 120,
                max_rate_limit_keys: 20_000,
                quorum_min_reporters: 2,
                degraded_single_reporter_enabled,
                raw_retention_days: 7,
                reporter_retention_days: 30,
                duplicate_ttl_secs: 300,
                max_seen_idempotency_keys: 50_000,
                max_reports_per_batch: 1024,
                max_territory_name_len: 96,
                max_idempotency_key_len: 128,
                malformed_threshold: 3,
                max_malformed_penalty_keys: 10_000,
                quarantine_secs: 600,
                max_pending_territories: 2048,
                max_claims_per_territory: 128,
                max_forward_queue: 4096,
                forward_max_attempts: 6,
                trusted_proxy_cidrs: Vec::new(),
                auth_required: true,
                single_reporter_mode: true,
                require_session_proof: true,
                session_refresh_interval_secs: 600,
                session_fail_open_grace_secs: 1800,
                allowed_server_host_suffixes: vec![".wynncraft.com".to_string()],
                world_attestation_max_age_secs: 120,
                max_signed_nonce_keys: 100_000,
                signed_nonce_window_secs: 300,
                owner_soft_corroboration: true,
                owner_corroboration_window_secs: 90,
                owner_revert_on_mismatch: true,
                active_reporter_stale_secs: 1800,
            },
            db,
            http: Client::new(),
            reporters: Arc::new(RwLock::new(HashMap::new())),
            token_index: Arc::new(RwLock::new(HashMap::new())),
            ip_windows: Arc::new(RwLock::new(HashMap::new())),
            reporter_windows: Arc::new(RwLock::new(HashMap::new())),
            malformed_penalties: Arc::new(RwLock::new(HashMap::new())),
            quarantined_until: Arc::new(RwLock::new(HashMap::new())),
            seen_idempotency: Arc::new(RwLock::new(HashMap::new())),
            pending_territory: Arc::new(RwLock::new(HashMap::new())),
            identities: Arc::new(RwLock::new(HashMap::new())),
            challenges: Arc::new(RwLock::new(HashMap::new())),
            seen_signed_nonces: Arc::new(RwLock::new(HashMap::new())),
            provisional_ownership: Arc::new(RwLock::new(HashMap::new())),
            session_verifier_fail_open_until: Arc::new(RwLock::new(None)),
            forward_queue: Arc::new(RwLock::new(VecDeque::new())),
            metrics: Arc::new(Metrics::default()),
        });

        if active_reporters > 0 {
            let mut reporters = state.reporters.write().await;
            for idx in 0..active_reporters {
                reporters.insert(
                    format!("active-reporter-{idx}"),
                    ReporterRecord {
                        token_hash: format!("token-hash-{idx}"),
                        token_expires_at: now + chrono::Duration::hours(1),
                        revoked: false,
                        guild_opt_in: false,
                        field_toggles: ReporterFieldToggles::default(),
                        last_seen: now,
                        device_pubkey_b64: format!("device-pubkey-{idx}"),
                        device_key_id: format!("device-key-{idx}"),
                        mojang_uuid: format!("uuid-{idx}"),
                        mojang_username: format!("user-{idx}"),
                        last_attested_at: now,
                    },
                );
            }
        }

        state
    }

    fn basic_claim_update() -> CanonicalTerritoryUpdate {
        CanonicalTerritoryUpdate {
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
            idempotency_key: None,
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
    fn session_verifier_fail_open_window_is_bounded_to_first_outage() {
        let start = Instant::now();
        let grace_secs = 3_u64;
        let mut fail_open_until = None;

        assert!(session_verifier_within_fail_open_grace(
            &mut fail_open_until,
            start,
            grace_secs,
        ));
        let first_deadline = fail_open_until.expect("deadline should be created");

        assert!(session_verifier_within_fail_open_grace(
            &mut fail_open_until,
            start + Duration::from_secs(2),
            grace_secs,
        ));
        assert!(!session_verifier_within_fail_open_grace(
            &mut fail_open_until,
            start + Duration::from_secs(4),
            grace_secs,
        ));
        assert_eq!(fail_open_until, Some(first_deadline));
    }

    #[test]
    fn quorum_requires_distinct_reporters_and_origins() {
        assert!(!quorum_satisfied(2, 1, 2));
        assert!(!quorum_satisfied(1, 2, 2));
        assert!(quorum_satisfied(2, 2, 2));
    }

    #[tokio::test]
    async fn degraded_mode_accepts_single_reporter_when_only_one_active() {
        let state = test_state_with_active_reporters(true, 1).await;
        let mut owner_only = basic_claim_update();
        owner_only.runtime = None;
        owner_only.guild = Some(GuildRef {
            uuid: "guild-uuid".to_string(),
            name: "Guild".to_string(),
            prefix: "GLD".to_string(),
            color: None,
        });
        owner_only.acquired = Some("2026-02-28T20:00:00Z".to_string());
        let decision = evaluate_territory_claim(
            &state,
            "reporter-a",
            IpAddr::from([203, 0, 113, 10]),
            owner_only,
        )
        .await;

        let (accepted, was_degraded, was_quorum) =
            decision.expect("single active reporter should be accepted in degraded mode");
        let provenance = accepted
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.provenance.as_ref())
            .expect("degraded acceptance should include provenance metadata");
        assert!(was_degraded);
        assert!(!was_quorum);
        assert_eq!(provenance.reporter_count, 1);
        assert_eq!(provenance.source, "fabric_reporter");
        assert!(!provenance.observed_at.is_empty());
    }

    #[tokio::test]
    async fn single_reporter_requires_quorum_when_degraded_mode_disabled() {
        let state = test_state_with_active_reporters(false, 1).await;
        let decision = evaluate_territory_claim(
            &state,
            "reporter-a",
            IpAddr::from([203, 0, 113, 10]),
            basic_claim_update(),
        )
        .await;

        assert!(
            decision.is_none(),
            "single reporter should be held pending when degraded mode is disabled"
        );
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

    #[tokio::test]
    async fn check_rate_limit_treats_zero_limit_as_disabled() {
        let windows: Arc<RwLock<HashMap<String, VecDeque<Instant>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        for _ in 0..100 {
            assert!(check_rate_limit(&windows, "shared-ip", 0, 1, Duration::from_secs(60)).await);
        }

        assert!(
            windows.read().await.is_empty(),
            "disabled limiter should not accumulate window state"
        );
    }

    #[tokio::test]
    async fn check_rate_limit_enforces_non_zero_limit() {
        let windows: Arc<RwLock<HashMap<String, VecDeque<Instant>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        assert!(check_rate_limit(&windows, "reporter-a", 2, 100, Duration::from_secs(60)).await);
        assert!(check_rate_limit(&windows, "reporter-a", 2, 100, Duration::from_secs(60)).await);
        assert!(!check_rate_limit(&windows, "reporter-a", 2, 100, Duration::from_secs(60)).await);
    }
}
