use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::Utc;
use futures::stream::{self, StreamExt};

use crate::config::{
    GUILD_CACHE_TTL_SECS, MAX_GUILD_CACHE_ENTRIES, WYNNCRAFT_GUILD_URL,
    guilds_online_cache_ttl_secs, guilds_online_max_concurrency,
};
use crate::state::{AppState, CachedGuild, ObservabilitySnapshot};

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const MAX_GUILD_NAME_LEN: usize = 64;

pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let territory_count = state.live_snapshot.read().await.territories.len();
    let observability = state.observability.snapshot();
    Json(serde_json::json!({
        "status": "ok",
        "territories": territory_count,
        "live_wars": 0,
        "guild_cache_size": state.guild_cache.len(),
        "history_available": state.db.is_some(),
        "seq_live_handoff_v1": state.seq_live_handoff_v1,
        "observability": {
            "live_state_requests_total": observability.live_state_requests_total,
            "persist_failures_total": observability.persist_failures_total,
            "dropped_update_events_total": observability.dropped_update_events_total,
            "persisted_update_events_total": observability.persisted_update_events_total,
            "guilds_online_requests_total": observability.guilds_online_requests_total,
            "guilds_online_cache_hits_total": observability.guilds_online_cache_hits_total,
            "guilds_online_cache_misses_total": observability.guilds_online_cache_misses_total,
            "guilds_online_upstream_errors_total": observability.guilds_online_upstream_errors_total,
            "ingest_reports_total": observability.ingest_reports_total,
            "ingest_reports_rejected_total": observability.ingest_reports_rejected_total,
            "ingest_reports_applied_total": observability.ingest_reports_applied_total,
            "ingest_reports_degraded_total": observability.ingest_reports_degraded_total,
        }
    }))
}

/// Serve pre-serialized TerritoryMap JSON — no HashMap clone, no re-serialization.
pub async fn get_territories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (etag, json): (String, Arc<Bytes>) = {
        let snapshot = state.live_snapshot.read().await;
        (
            territories_etag(snapshot.seq),
            Arc::clone(&snapshot.territories_json),
        )
    };

    if if_none_match_matches(&headers, &etag) {
        return not_modified_response("public, max-age=5", Some(etag.as_str()));
    }

    json_bytes_response((*json).clone(), "public, max-age=5", Some(etag.as_str()))
}

pub async fn get_live_state(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.observability.record_live_state_request();
    let (etag, json): (String, Arc<Bytes>) = {
        let snapshot = state.live_snapshot.read().await;
        (
            live_state_etag(snapshot.seq),
            Arc::clone(&snapshot.live_state_json),
        )
    };

    if if_none_match_matches(&headers, &etag) {
        return not_modified_response("public, max-age=5", Some(etag.as_str()));
    }

    json_bytes_response((*json).clone(), "public, max-age=5", Some(etag.as_str()))
}

pub async fn get_season_scalar_current(State(state): State<AppState>) -> impl IntoResponse {
    let cached = {
        let latest = state.latest_scalar_sample.read().await;
        latest.as_ref().map(|(_, json)| Arc::clone(json))
    };
    let body = match cached {
        Some(json) => (*json).clone(),
        None => Bytes::from_static(br#"{"sample":null}"#),
    };

    json_bytes_response(body, "public, max-age=60", None)
}

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let territory_count = state.live_snapshot.read().await.territories.len();
    let guild_cache_size = state.guild_cache.len();
    let history_available = state.db.is_some();
    let seq_live_handoff_v1 = state.seq_live_handoff_v1;
    let observability = state.observability.snapshot();

    let body = render_prometheus_metrics(
        territory_count,
        guild_cache_size,
        history_available,
        seq_live_handoff_v1,
        observability,
    );

    (
        [
            (header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
}

fn render_prometheus_metrics(
    territory_count: usize,
    guild_cache_size: usize,
    history_available: bool,
    seq_live_handoff_v1: bool,
    observability: ObservabilitySnapshot,
) -> String {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "# HELP sequoia_territories Current number of territories in the live snapshot."
    );
    let _ = writeln!(body, "# TYPE sequoia_territories gauge");
    let _ = writeln!(body, "sequoia_territories {territory_count}");

    let _ = writeln!(
        body,
        "# HELP sequoia_guild_cache_size Current number of guild entries in cache."
    );
    let _ = writeln!(body, "# TYPE sequoia_guild_cache_size gauge");
    let _ = writeln!(body, "sequoia_guild_cache_size {guild_cache_size}");

    let _ = writeln!(
        body,
        "# HELP sequoia_history_available Whether history storage is available (1 or 0)."
    );
    let _ = writeln!(body, "# TYPE sequoia_history_available gauge");
    let _ = writeln!(
        body,
        "sequoia_history_available {}",
        u8::from(history_available)
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_seq_live_handoff_v1_enabled Whether seq live handoff v1 is enabled (1 or 0)."
    );
    let _ = writeln!(body, "# TYPE sequoia_seq_live_handoff_v1_enabled gauge");
    let _ = writeln!(
        body,
        "sequoia_seq_live_handoff_v1_enabled {}",
        u8::from(seq_live_handoff_v1)
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_live_state_requests_total Total live-state API requests."
    );
    let _ = writeln!(body, "# TYPE sequoia_live_state_requests_total counter");
    let _ = writeln!(
        body,
        "sequoia_live_state_requests_total {}",
        observability.live_state_requests_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_persist_failures_total Total failures while persisting update events."
    );
    let _ = writeln!(body, "# TYPE sequoia_persist_failures_total counter");
    let _ = writeln!(
        body,
        "sequoia_persist_failures_total {}",
        observability.persist_failures_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_dropped_update_events_total Total update events dropped before broadcast."
    );
    let _ = writeln!(body, "# TYPE sequoia_dropped_update_events_total counter");
    let _ = writeln!(
        body,
        "sequoia_dropped_update_events_total {}",
        observability.dropped_update_events_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_persisted_update_events_total Total update events persisted before broadcast."
    );
    let _ = writeln!(body, "# TYPE sequoia_persisted_update_events_total counter");
    let _ = writeln!(
        body,
        "sequoia_persisted_update_events_total {}",
        observability.persisted_update_events_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_guilds_online_requests_total Total /api/guilds/online requests."
    );
    let _ = writeln!(body, "# TYPE sequoia_guilds_online_requests_total counter");
    let _ = writeln!(
        body,
        "sequoia_guilds_online_requests_total {}",
        observability.guilds_online_requests_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_guilds_online_cache_hits_total Total guild rows served from cache by /api/guilds/online."
    );
    let _ = writeln!(
        body,
        "# TYPE sequoia_guilds_online_cache_hits_total counter"
    );
    let _ = writeln!(
        body,
        "sequoia_guilds_online_cache_hits_total {}",
        observability.guilds_online_cache_hits_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_guilds_online_cache_misses_total Total guild rows fetched upstream by /api/guilds/online."
    );
    let _ = writeln!(
        body,
        "# TYPE sequoia_guilds_online_cache_misses_total counter"
    );
    let _ = writeln!(
        body,
        "sequoia_guilds_online_cache_misses_total {}",
        observability.guilds_online_cache_misses_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_guilds_online_upstream_errors_total Total upstream failures while serving /api/guilds/online."
    );
    let _ = writeln!(
        body,
        "# TYPE sequoia_guilds_online_upstream_errors_total counter"
    );
    let _ = writeln!(
        body,
        "sequoia_guilds_online_upstream_errors_total {}",
        observability.guilds_online_upstream_errors_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_ingest_reports_total Total internal ingest reports received."
    );
    let _ = writeln!(body, "# TYPE sequoia_ingest_reports_total counter");
    let _ = writeln!(
        body,
        "sequoia_ingest_reports_total {}",
        observability.ingest_reports_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_ingest_reports_rejected_total Total internal ingest reports rejected."
    );
    let _ = writeln!(body, "# TYPE sequoia_ingest_reports_rejected_total counter");
    let _ = writeln!(
        body,
        "sequoia_ingest_reports_rejected_total {}",
        observability.ingest_reports_rejected_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_ingest_reports_applied_total Total internal ingest reports applied."
    );
    let _ = writeln!(body, "# TYPE sequoia_ingest_reports_applied_total counter");
    let _ = writeln!(
        body,
        "sequoia_ingest_reports_applied_total {}",
        observability.ingest_reports_applied_total
    );

    let _ = writeln!(
        body,
        "# HELP sequoia_ingest_reports_degraded_total Total internal ingest reports applied in degraded confidence mode."
    );
    let _ = writeln!(body, "# TYPE sequoia_ingest_reports_degraded_total counter");
    let _ = writeln!(
        body,
        "sequoia_ingest_reports_degraded_total {}",
        observability.ingest_reports_degraded_total
    );

    body
}

pub async fn get_guild(
    State(state): State<AppState>,
    Path(raw_name): Path<String>,
) -> Result<Response, StatusCode> {
    let name = normalize_guild_name(&raw_name)?.to_owned();

    // Check cache
    if let Some(cached) = state.guild_cache.get(&name) {
        let age = Utc::now()
            .signed_duration_since(cached.fetched_at)
            .num_seconds();
        if age < GUILD_CACHE_TTL_SECS {
            return Ok(json_bytes_response(
                Bytes::from(cached.data.clone()),
                "public, max-age=300",
                None,
            ));
        }
    }

    // Fetch from Wynncraft API
    let url = guild_details_url(&name)?;
    let resp = state
        .http_client
        .get(url)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !resp.status().is_success() {
        return Err(StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    }

    let data = resp.text().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    cache_guild_payload(&state, name, data.clone());

    Ok(json_bytes_response(
        Bytes::from(data),
        "public, max-age=300",
        None,
    ))
}

const MAX_GUILDS_ONLINE_BATCH: usize = 25;

#[derive(serde::Deserialize)]
pub struct GuildsOnlineQuery {
    #[serde(default)]
    pub names: String,
}

#[derive(serde::Serialize)]
pub struct GuildOnlineEntry {
    pub online: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_rating: Option<i64>,
}

pub async fn get_guilds_online(
    State(state): State<AppState>,
    Query(query): Query<GuildsOnlineQuery>,
) -> Result<Json<HashMap<String, GuildOnlineEntry>>, StatusCode> {
    state.observability.record_guilds_online_request();

    let guild_names = parse_guilds_online_names(&query.names);

    if guild_names.is_empty() {
        return Ok(Json(HashMap::new()));
    }

    if guild_names.len() > MAX_GUILDS_ONLINE_BATCH {
        return Err(StatusCode::BAD_REQUEST);
    }

    let now = Utc::now();
    let online_ttl_secs = guilds_online_cache_ttl_secs();
    let mut cache_hits = 0_u64;
    let mut cache_misses = 0_u64;
    let mut result = HashMap::new();
    let mut to_fetch = Vec::new();

    for name in guild_names {
        if let Some(cached) = state.guild_cache.get(&name) {
            let age = now.signed_duration_since(cached.fetched_at).num_seconds();
            if age < online_ttl_secs
                && let Some(entry) = parse_guild_online_entry(&cached.data)
            {
                result.insert(name, entry);
                cache_hits += 1;
                continue;
            }
        }

        cache_misses += 1;
        to_fetch.push(name);
    }

    if cache_hits > 0 {
        state
            .observability
            .record_guilds_online_cache_hits(cache_hits);
    }
    if cache_misses > 0 {
        state
            .observability
            .record_guilds_online_cache_misses(cache_misses);
    }

    if !to_fetch.is_empty() {
        let max_concurrency = guilds_online_max_concurrency().clamp(1, MAX_GUILDS_ONLINE_BATCH);

        let fetched = stream::iter(to_fetch.into_iter().map(|name| {
            let state = state.clone();
            async move {
                let url = match guild_details_url(&name) {
                    Ok(url) => url,
                    Err(_) => return (name, None, true),
                };

                let resp = match state.http_client.get(url).send().await {
                    Ok(resp) if resp.status().is_success() => resp,
                    _ => return (name, None, true),
                };

                let data = match resp.text().await {
                    Ok(data) => data,
                    Err(_) => return (name, None, true),
                };

                let entry = parse_guild_online_entry(&data);
                let entry_missing = entry.is_none();
                cache_guild_payload(&state, name.clone(), data);
                (name, entry, entry_missing)
            }
        }))
        .buffer_unordered(max_concurrency)
        .collect::<Vec<_>>()
        .await;

        let mut upstream_errors = 0_u64;
        for (name, entry, errored) in fetched {
            if let Some(entry) = entry {
                result.insert(name, entry);
            }
            if errored {
                upstream_errors += 1;
            }
        }

        if upstream_errors > 0 {
            state
                .observability
                .record_guilds_online_upstream_errors(upstream_errors);
        }
    }

    Ok(Json(result))
}

fn parse_guilds_online_names(raw_names: &str) -> Vec<String> {
    let mut unique = HashSet::new();
    raw_names
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|name| normalize_guild_name(name).ok().map(str::to_owned))
        .filter(|name| unique.insert(name.clone()))
        .collect()
}

fn parse_guild_online_entry(json_str: &str) -> Option<GuildOnlineEntry> {
    let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let online = val.get("online")?.as_u64()? as u32;
    let season_rating = val
        .get("seasonRanks")
        .and_then(|v| v.as_object())
        .and_then(|ranks| {
            ranks
                .iter()
                .filter_map(|(k, v)| k.parse::<i32>().ok().map(|id| (id, v)))
                .max_by_key(|(id, _)| *id)
        })
        .and_then(|(_, v)| v.get("rating").and_then(|r| r.as_i64()));
    Some(GuildOnlineEntry {
        online,
        season_rating,
    })
}

fn normalize_guild_name(name: &str) -> Result<&str, StatusCode> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_GUILD_NAME_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }

    if trimmed
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\' | '?' | '#'))
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok(trimmed)
}

fn guild_details_url(guild_name: &str) -> Result<reqwest::Url, StatusCode> {
    let mut url =
        reqwest::Url::parse(WYNNCRAFT_GUILD_URL).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Ok(mut path_segments) = url.path_segments_mut() else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };
    path_segments.push(guild_name);
    drop(path_segments);
    Ok(url)
}

fn cache_guild_payload(state: &AppState, name: String, data: String) {
    if !state.guild_cache.contains_key(&name) {
        while state.guild_cache.len() >= MAX_GUILD_CACHE_ENTRIES {
            if !evict_oldest_guild_entry(state) {
                break;
            }
        }
    }

    state.guild_cache.insert(
        name,
        CachedGuild {
            data,
            fetched_at: Utc::now(),
        },
    );
}

fn evict_oldest_guild_entry(state: &AppState) -> bool {
    let Some(oldest_name) = state
        .guild_cache
        .iter()
        .min_by_key(|entry| entry.value().fetched_at)
        .map(|entry| entry.key().clone())
    else {
        return false;
    };
    state.guild_cache.remove(&oldest_name).is_some()
}

fn territories_etag(seq: u64) -> String {
    format!("\"territories-{seq}\"")
}

fn live_state_etag(seq: u64) -> String {
    format!("\"live-state-{seq}\"")
}

fn json_bytes_response(body: Bytes, cache_control: &'static str, etag: Option<&str>) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    if let Some(etag) = etag
        && let Ok(etag_header) = HeaderValue::from_str(etag)
    {
        headers.insert(header::ETAG, etag_header);
    }
    response
}

fn not_modified_response(cache_control: &'static str, etag: Option<&str>) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    if let Some(etag) = etag
        && let Ok(etag_header) = HeaderValue::from_str(etag)
    {
        headers.insert(header::ETAG, etag_header);
    }
    response
}

fn normalize_etag(candidate: &str) -> &str {
    candidate.strip_prefix("W/").unwrap_or(candidate).trim()
}

fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_NONE_MATCH) else {
        return false;
    };
    let Ok(raw) = value.to_str() else {
        return false;
    };

    raw.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || normalize_etag(candidate) == normalize_etag(etag)
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use chrono::Utc;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use super::{
        StatusCode, guild_details_url, if_none_match_matches, normalize_guild_name,
        parse_guild_online_entry, parse_guilds_online_names, render_prometheus_metrics,
    };
    use crate::state::{AppState, ObservabilitySnapshot};
    use sequoia_shared::{SeasonScalarCurrent, SeasonScalarSample};
    use sqlx::postgres::PgPoolOptions;

    const REAL_DB_TEST_LOCK: i64 = 73_019_001;

    async fn spawn_test_server(state: AppState) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let app = crate::app::build_app(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        (addr, handle)
    }

    #[test]
    fn metrics_output_contains_prometheus_help_type_and_values() {
        let observability = ObservabilitySnapshot {
            live_state_requests_total: 12,
            persist_failures_total: 3,
            dropped_update_events_total: 7,
            persisted_update_events_total: 99,
            guilds_online_requests_total: 5,
            guilds_online_cache_hits_total: 8,
            guilds_online_cache_misses_total: 2,
            guilds_online_upstream_errors_total: 1,
            ingest_reports_total: 11,
            ingest_reports_rejected_total: 2,
            ingest_reports_applied_total: 8,
            ingest_reports_degraded_total: 1,
        };

        let metrics = render_prometheus_metrics(42, 5, true, false, observability);

        assert!(metrics.contains("# HELP sequoia_territories"));
        assert!(metrics.contains("# TYPE sequoia_live_state_requests_total counter"));
        assert!(metrics.contains("sequoia_territories 42"));
        assert!(metrics.contains("sequoia_guild_cache_size 5"));
        assert!(metrics.contains("sequoia_history_available 1"));
        assert!(metrics.contains("sequoia_seq_live_handoff_v1_enabled 0"));
        assert!(metrics.contains("sequoia_live_state_requests_total 12"));
        assert!(metrics.contains("sequoia_persist_failures_total 3"));
        assert!(metrics.contains("sequoia_dropped_update_events_total 7"));
        assert!(metrics.contains("sequoia_persisted_update_events_total 99"));
        assert!(metrics.contains("sequoia_guilds_online_requests_total 5"));
        assert!(metrics.contains("sequoia_guilds_online_cache_hits_total 8"));
        assert!(metrics.contains("sequoia_guilds_online_cache_misses_total 2"));
        assert!(metrics.contains("sequoia_guilds_online_upstream_errors_total 1"));
        assert!(metrics.contains("sequoia_ingest_reports_total 11"));
        assert!(metrics.contains("sequoia_ingest_reports_rejected_total 2"));
        assert!(metrics.contains("sequoia_ingest_reports_applied_total 8"));
        assert!(metrics.contains("sequoia_ingest_reports_degraded_total 1"));
    }

    #[test]
    fn normalize_guild_name_rejects_invalid_inputs() {
        assert_eq!(normalize_guild_name(""), Err(StatusCode::BAD_REQUEST));
        assert_eq!(normalize_guild_name("   "), Err(StatusCode::BAD_REQUEST));
        assert_eq!(
            normalize_guild_name("Guild/Name"),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_guild_name("Guild?name"),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_guild_name("Guild#name"),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            normalize_guild_name("Guild\\name"),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn parse_guilds_online_names_deduplicates_and_filters_invalid() {
        let parsed = parse_guilds_online_names("GuildOne, GuildTwo, GuildOne, Guild/Bad,  ");
        assert_eq!(parsed, vec!["GuildOne".to_string(), "GuildTwo".to_string()]);
    }

    #[test]
    fn parse_guild_online_entry_uses_latest_season_rating() {
        let payload = r#"{
            "online": 14,
            "seasonRanks": {
                "29": {"rating": 9000},
                "31": {"rating": 12000},
                "oops": {"rating": 999999}
            }
        }"#;

        let entry = parse_guild_online_entry(payload).expect("guild payload should parse");
        assert_eq!(entry.online, 14);
        assert_eq!(entry.season_rating, Some(12000));
    }

    #[test]
    fn guild_details_url_percent_encodes_path_segments() {
        let url = guild_details_url("The Guild")
            .expect("guild URL should be created for valid guild names");
        assert_eq!(
            url.as_str(),
            "https://api.wynncraft.com/v3/guild/The%20Guild"
        );
    }

    #[tokio::test]
    async fn health_and_metrics_expose_expected_contract() {
        let state = AppState::new(None);
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        client
            .get(format!("{base_url}/api/live/state"))
            .send()
            .await
            .expect("live-state request")
            .error_for_status()
            .expect("live-state status");

        let health = client
            .get(format!("{base_url}/api/health"))
            .send()
            .await
            .expect("health request")
            .error_for_status()
            .expect("health status")
            .json::<serde_json::Value>()
            .await
            .expect("parse health");

        assert_eq!(health.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(
            health.get("history_available").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(
            health
                .get("observability")
                .and_then(|v| v.get("live_state_requests_total"))
                .and_then(|v| v.as_u64())
                .is_some()
        );
        assert!(
            health
                .get("observability")
                .and_then(|v| v.get("guilds_online_requests_total"))
                .and_then(|v| v.as_u64())
                .is_some()
        );

        let metrics = client
            .get(format!("{base_url}/api/metrics"))
            .send()
            .await
            .expect("metrics request")
            .error_for_status()
            .expect("metrics status")
            .text()
            .await
            .expect("parse metrics text");

        assert!(metrics.contains("# TYPE sequoia_live_state_requests_total counter"));
        assert!(metrics.contains("# TYPE sequoia_history_available gauge"));
        assert!(metrics.contains("sequoia_live_state_requests_total 1"));
        assert!(metrics.contains("sequoia_history_available 0"));
        assert!(metrics.contains("sequoia_guilds_online_requests_total 0"));

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn season_scalar_endpoint_returns_null_sample_without_db() {
        let state = AppState::new(None);
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");

        let sample = reqwest::Client::new()
            .get(format!("{base_url}/api/season/scalar/current"))
            .send()
            .await
            .expect("season scalar request")
            .error_for_status()
            .expect("season scalar status")
            .json::<SeasonScalarCurrent>()
            .await
            .expect("parse season scalar response");

        assert!(sample.sample.is_none());

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn season_scalar_endpoint_returns_latest_persisted_sample() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("Skipping season scalar API test: DATABASE_URL is not set");
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
            .expect("acquire db lock");

        crate::db_migrations::run(&pool)
            .await
            .expect("run migrations");
        sqlx::query(
            "TRUNCATE TABLE territory_events, territory_snapshots, season_scalar_samples, season_guild_observations, guild_color_cache RESTART IDENTITY",
        )
        .execute(&pool)
        .await
        .expect("truncate tables");

        let older = Utc::now() - chrono::TimeDelta::minutes(10);
        let newer = Utc::now() - chrono::TimeDelta::minutes(1);
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(older)
        .bind(28_i32)
        .bind(1.6_f64)
        .bind(1.9_f64)
        .bind(0.41_f64)
        .bind(3_i32)
        .execute(&pool)
        .await
        .expect("insert older sample");
        sqlx::query(
            "INSERT INTO season_scalar_samples \
             (sampled_at, season_id, scalar_weighted, scalar_raw, confidence, sample_count) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(newer)
        .bind(29_i32)
        .bind(2.2_f64)
        .bind(2.5_f64)
        .bind(0.77_f64)
        .bind(6_i32)
        .execute(&pool)
        .await
        .expect("insert newer sample");

        let state = AppState::new(Some(pool));
        {
            let sample = SeasonScalarSample {
                sampled_at: newer.to_rfc3339(),
                season_id: 29,
                scalar_weighted: 2.2,
                scalar_raw: 2.5,
                confidence: 0.77,
                sample_count: 6,
            };
            let payload = serde_json::to_vec(&SeasonScalarCurrent {
                sample: Some(sample.clone()),
            })
            .expect("serialize scalar payload");
            let mut latest = state.latest_scalar_sample.write().await;
            *latest = Some((sample, Arc::new(Bytes::from(payload))));
        }
        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");

        let payload = reqwest::Client::new()
            .get(format!("{base_url}/api/season/scalar/current"))
            .send()
            .await
            .expect("season scalar request")
            .error_for_status()
            .expect("season scalar status")
            .json::<SeasonScalarCurrent>()
            .await
            .expect("parse season scalar response");

        let sample = payload.sample.expect("latest sample should be present");
        assert_eq!(sample.season_id, 29);
        assert_eq!(sample.sample_count, 6);
        assert!((sample.scalar_weighted - 2.2).abs() < 1e-9);
        assert!((sample.scalar_raw - 2.5).abs() < 1e-9);

        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(REAL_DB_TEST_LOCK)
            .execute(&mut *lock_conn)
            .await
            .expect("release db lock");

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[test]
    fn if_none_match_supports_weak_and_multiple_etags() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::IF_NONE_MATCH,
            axum::http::HeaderValue::from_static("W/\"other\", \"territories-42\""),
        );
        assert!(if_none_match_matches(&headers, "\"territories-42\""));
    }

    #[tokio::test]
    async fn territories_endpoint_returns_not_modified_when_etag_matches() {
        let state = AppState::new(None);
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.seq = 9;
            snapshot.territories_json = Arc::new(Bytes::from_static(b"{\"Alpha\":{}}"));
        }

        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let first = client
            .get(format!("{base_url}/api/territories"))
            .send()
            .await
            .expect("territories request should succeed");
        let first_status = first.status();
        let first_etag = first
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .expect("etag header should be present");
        let first_body = first.text().await.expect("read first response body");

        assert_eq!(first_status, reqwest::StatusCode::OK);
        assert_eq!(first_body, "{\"Alpha\":{}}");

        let second = client
            .get(format!("{base_url}/api/territories"))
            .header(reqwest::header::IF_NONE_MATCH, first_etag)
            .send()
            .await
            .expect("conditional territories request should succeed");

        assert_eq!(second.status(), reqwest::StatusCode::NOT_MODIFIED);
        assert_eq!(
            second
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=5")
        );

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn live_state_endpoint_returns_not_modified_when_etag_matches() {
        let state = AppState::new(None);
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.seq = 11;
            snapshot.live_state_json = Arc::new(Bytes::from_static(
                b"{\"seq\":11,\"timestamp\":\"2026-01-01T00:00:00Z\",\"territories\":{\"Alpha\":{}}}",
            ));
        }

        let (addr, server_handle) = spawn_test_server(state).await;
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();

        let first = client
            .get(format!("{base_url}/api/live/state"))
            .send()
            .await
            .expect("live-state request should succeed");
        let first_status = first.status();
        let first_etag = first
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .expect("etag header should be present");
        let first_body = first.text().await.expect("read first response body");

        assert_eq!(first_status, reqwest::StatusCode::OK);
        let parsed: serde_json::Value =
            serde_json::from_str(&first_body).expect("live-state response should be JSON");
        assert_eq!(parsed["seq"], 11);
        assert!(parsed["territories"].is_object());

        let second = client
            .get(format!("{base_url}/api/live/state"))
            .header(reqwest::header::IF_NONE_MATCH, first_etag)
            .send()
            .await
            .expect("conditional live-state request should succeed");

        assert_eq!(second.status(), reqwest::StatusCode::NOT_MODIFIED);
        assert_eq!(
            second
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=5")
        );

        server_handle.abort();
        let _ = server_handle.await;
    }
}
