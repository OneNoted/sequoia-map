use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use tracing::warn;

use super::http_util::{
    if_none_match_matches, json_bytes_response, not_modified_response, private_json_bytes_response,
};

use crate::config::{
    MAX_GUILD_CACHE_ENTRIES, guilds_online_cache_ttl_secs, guilds_online_max_concurrency,
};
use crate::services::season_data::{self, SeasonDataError};
use crate::services::season_race::{self, SeasonRaceError};
use crate::services::wynncraft_api;
use crate::state::{AppState, CachedGuild, ObservabilitySnapshot};

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const MAX_GUILD_NAME_LEN: usize = 64;
type LatestObservedRatingRow = (String, i32, DateTime<Utc>, i32);

pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let territory_count = state.live_snapshot.read().await.territories.len();
    let observability = state.observability.snapshot();
    Json(serde_json::json!({
        "status": "ok",
        "territories": territory_count,
        "live_wars": 0,
        "guild_cache_size": state.guild_cache.len(),
        "history_available": state.db.is_some(),
        "claims_persistence_available": state.db.is_some(),
        "seq_live_handoff_v1": state.seq_live_handoff_v1,
        "observability": {
            "live_state_requests_total": observability.live_state_requests_total,
            "persist_failures_total": observability.persist_failures_total,
            "dropped_update_events_total": observability.dropped_update_events_total,
            "dropped_archive_reports_total": observability.dropped_archive_reports_total,
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

#[derive(serde::Deserialize)]
pub struct SeasonRaceQuery {
    #[serde(default)]
    pub season_id: Option<i32>,
}

#[derive(serde::Deserialize)]
pub struct SeasonSeriesQuery {
    #[serde(default)]
    pub season_id: Option<i32>,
    #[serde(default)]
    pub guild_names: Option<String>,
}

pub async fn get_season_windows(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let response = season_data::build_windows_response(&state)
        .await
        .map_err(map_season_data_status)?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok((headers, Json(response)))
}

pub async fn get_season_series(
    State(state): State<AppState>,
    Query(query): Query<SeasonSeriesQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let guild_names = query
        .guild_names
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let response = season_data::build_series_response(&state, query.season_id, &guild_names)
        .await
        .map_err(map_season_data_status)?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    Ok((headers, Json(response)))
}

pub async fn get_season_race(
    State(state): State<AppState>,
    Query(query): Query<SeasonRaceQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let response = season_race::build_race_response(&state, query.season_id)
        .await
        .map_err(|error| match error {
            SeasonRaceError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            SeasonRaceError::BadRequest => StatusCode::BAD_REQUEST,
            SeasonRaceError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    Ok((headers, Json(response)))
}

/// The war feed is internal guild intel, so it is served only to a viewer with war access
/// ([`super::war_access`]). Everyone else is refused outright rather than handed an empty
/// state, so the client can tell "no wars" apart from "not yours to see".
pub async fn get_warcontroller(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // An answer that could not be had must not read to the client as a refusal: 503 is
    // retryable, 403 would have the map hide the feed for good.
    if let Err(status) = super::war_access::war_access(&state, &headers)
        .await
        .into_result()
    {
        return status.into_response();
    }

    let cached = {
        let cache = state.warcontroller_cache.read().await;
        cache.as_ref().map(|cached| Arc::clone(&cached.json))
    };
    let body = match cached {
        Some(json) => (*json).clone(),
        None => Bytes::from_static(
            br#"{"timestamp":0,"queues":[],"wars":[],"players":[],"unavailable":true}"#,
        ),
    };

    private_json_bytes_response(body)
}

pub async fn get_map_intel(State(state): State<AppState>) -> Result<impl IntoResponse, StatusCode> {
    let response = wynncraft_api::cached_map_intel_summary(&state)
        .await
        .map_err(|error| {
            warn!(error = %error, "failed to load Wynncraft map intel");
            StatusCode::BAD_GATEWAY
        })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    Ok((headers, Json(response)))
}

pub async fn get_map_intel_overlay(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let response = wynncraft_api::cached_map_intel_overlay(&state)
        .await
        .map_err(|error| {
            warn!(error = %error, "failed to load Wynncraft map intel overlay");
            StatusCode::BAD_GATEWAY
        })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    Ok((headers, Json(response)))
}

fn map_season_data_status(error: SeasonDataError) -> StatusCode {
    match error {
        SeasonDataError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        SeasonDataError::BadRequest => StatusCode::BAD_REQUEST,
        SeasonDataError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
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
        "# HELP sequoia_dropped_archive_reports_total Canonical archive reports dropped while the database was unavailable."
    );
    let _ = writeln!(body, "# TYPE sequoia_dropped_archive_reports_total counter");
    let _ = writeln!(
        body,
        "sequoia_dropped_archive_reports_total {}",
        observability.dropped_archive_reports_total
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
    // The panel lists who is online, so it is held no longer than the online counts are.
    let data = guild_payload(&state, name, guilds_online_cache_ttl_secs()).await?;
    Ok(json_bytes_response(
        Bytes::from(data),
        "public, max-age=60",
        None,
    ))
}

/// Wynncraft's guild payload, from the guild cache while it is at most `max_age_secs` old.
pub(crate) async fn guild_payload(
    state: &AppState,
    name: String,
    max_age_secs: i64,
) -> Result<String, StatusCode> {
    if let Some(cached) = state.guild_cache.get(&name) {
        let age = Utc::now()
            .signed_duration_since(cached.fetched_at)
            .num_seconds();
        if age < max_age_secs {
            return Ok(cached.data.clone());
        }
    }

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

    let observed_at = wynncraft_api::observed_at(&resp);
    let data = resp.text().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    cache_guild_payload(state, name, data.clone(), observed_at);
    Ok(data)
}

const MAX_GUILDS_ONLINE_BATCH: usize = 25;

#[derive(serde::Deserialize)]
pub struct GuildsOnlineQuery {
    #[serde(default)]
    pub names: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildSeasonRatingSource {
    Leaderboard,
    Live,
    SnapshotFallback,
}

#[derive(serde::Serialize)]
pub struct GuildOnlineEntry {
    pub online: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_rating: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_rank: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_rating_source: Option<GuildSeasonRatingSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_rating_sampled_at: Option<String>,
    /// The guild endpoint's rating for each season it lists.
    #[serde(skip)]
    pub own_ratings: HashMap<i32, i64>,
    /// When Wynncraft produced those ratings, to set against the leaderboard's.
    #[serde(skip)]
    pub own_observed_at: DateTime<Utc>,
}

pub async fn get_guilds_online(
    State(state): State<AppState>,
    Query(query): Query<GuildsOnlineQuery>,
) -> Result<(HeaderMap, Json<HashMap<String, GuildOnlineEntry>>), StatusCode> {
    state.observability.record_guilds_online_request();

    let guild_names = parse_guilds_online_names(&query.names);

    if guild_names.is_empty() {
        return Ok((HeaderMap::new(), Json(HashMap::new())));
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
                && let Some(entry) = parse_guild_online_entry(&cached.data, cached.observed_at)
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

    let max_concurrency = guilds_online_max_concurrency().clamp(1, MAX_GUILDS_ONLINE_BATCH);
    let fetch_missing = stream::iter(to_fetch.into_iter().map(|name| {
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

            let observed_at = wynncraft_api::observed_at(&resp);
            let data = match resp.text().await {
                Ok(data) => data,
                Err(_) => return (name, None, true),
            };

            let entry = parse_guild_online_entry(&data, observed_at);
            let entry_missing = entry.is_none();
            cache_guild_payload(&state, name.clone(), data, observed_at);
            (name, entry, entry_missing)
        }
    }))
    .buffer_unordered(max_concurrency)
    .collect::<Vec<_>>();
    // Leaderboard and season refreshes are more upstream requests; they need not wait for the guilds.
    let (fetched, leaderboard, seasons) = tokio::join!(
        fetch_missing,
        wynncraft_api::season_leaderboard_without_waiting(&state),
        wynncraft_api::guild_seasons_without_waiting(&state)
    );

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

    let leaderboard = leaderboard.unwrap_or_else(|error| {
        warn!(error = %error, "failed to load guild season leaderboard ratings");
        None
    });
    // Only Wynncraft's season list and leaderboard say which season is on, never one guild.
    let current_season = wynncraft_api::latest_begun_season(&seasons, now)
        .into_iter()
        .chain(
            leaderboard
                .as_ref()
                .map(|leaderboard| leaderboard.season_id),
        )
        .max();
    // Without a season Wynncraft vouches for, no rating can be called this season's. The next
    // batch asks Wynncraft again, so the client need not wait a whole poll for its ratings.
    let Some(current_season) = current_season else {
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("15"));
        return Ok((headers, Json(result)));
    };
    resolve_season_ratings(&mut result, leaderboard.as_ref(), current_season);

    if let Some(pool) = state.db.as_ref() {
        let missing_rating_names: Vec<String> = result
            .iter()
            .filter(|(_, entry)| entry.season_rating.is_none())
            .map(|(name, _)| name.clone())
            .collect();
        if !missing_rating_names.is_empty() {
            let fallback = match load_latest_observed_season_ratings(
                pool,
                &missing_rating_names,
                current_season,
            )
            .await
            {
                Ok(fallback) => fallback,
                Err(error) => {
                    warn!(
                        error = %error,
                        guild_count = missing_rating_names.len(),
                        "failed to load snapshot season rating fallback"
                    );
                    HashMap::new()
                }
            };
            apply_snapshot_fallback(&mut result, fallback);
        }
    }

    Ok((HeaderMap::new(), Json(result)))
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

fn parse_guild_online_entry(
    json_str: &str,
    observed_at: DateTime<Utc>,
) -> Option<GuildOnlineEntry> {
    let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let online = val.get("online")?.as_u64()? as u32;
    let own_ratings = val
        .get("seasonRanks")
        .and_then(|v| v.as_object())
        .map(|ranks| {
            ranks
                .iter()
                .filter_map(|(season_id, rank)| {
                    Some((
                        season_id.parse::<i32>().ok()?,
                        rank.get("rating")?.as_i64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(GuildOnlineEntry {
        online,
        season_rating: None,
        season_rank: None,
        season_id: None,
        season_rating_source: None,
        season_rating_sampled_at: None,
        own_ratings,
        own_observed_at: observed_at,
    })
}

/// Each guild's latest rating for `current_season`, the only season whose ratings are shown.
/// Between a guild's own rating and its leaderboard score for it, the later of Wynncraft's
/// two observations stands.
fn resolve_season_ratings(
    result: &mut HashMap<String, GuildOnlineEntry>,
    leaderboard: Option<&crate::state::CachedSeasonLeaderboard>,
    current_season: i32,
) {
    let listed = leaderboard
        .filter(|leaderboard| leaderboard.season_id == current_season)
        .map(|leaderboard| {
            let by_name = leaderboard
                .entries
                .iter()
                .map(|entry| (entry.name.to_ascii_lowercase(), entry))
                .collect::<HashMap<_, _>>();
            (leaderboard, by_name)
        });
    for (guild_name, entry) in result.iter_mut() {
        let own = entry
            .own_ratings
            .get(&current_season)
            .map(|&rating| (rating, entry.own_observed_at));
        let on_board = listed.as_ref().and_then(|(leaderboard, by_name)| {
            by_name
                .get(&guild_name.to_ascii_lowercase())
                .map(|listed| (*leaderboard, *listed))
        });
        entry.season_rank = on_board.map(|(_, listed)| listed.rank);
        let (rating, source, observed_at) = match (own, on_board) {
            (Some((rating, own_at)), Some((leaderboard, _)))
                if own_at >= leaderboard.observed_at =>
            {
                (rating, GuildSeasonRatingSource::Live, own_at)
            }
            (_, Some((leaderboard, listed))) => (
                listed.score,
                GuildSeasonRatingSource::Leaderboard,
                leaderboard.observed_at,
            ),
            (Some((rating, own_at)), None) => (rating, GuildSeasonRatingSource::Live, own_at),
            (None, None) => continue,
        };
        entry.season_rating = Some(rating);
        entry.season_id = Some(current_season);
        entry.season_rating_source = Some(source);
        entry.season_rating_sampled_at = Some(observed_at.to_rfc3339());
    }
}

async fn load_latest_observed_season_ratings(
    pool: &sqlx::PgPool,
    guild_names: &[String],
    season_id: i32,
) -> Result<HashMap<String, (i32, DateTime<Utc>, i64)>, String> {
    if guild_names.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<LatestObservedRatingRow> = sqlx::query_as(
        "SELECT guild_name, season_id, observed_at, season_rating \
         FROM ( \
             SELECT DISTINCT ON (guild_name) guild_name, season_id, observed_at, season_rating \
             FROM season_guild_observations \
             WHERE season_id = $2 \
               AND guild_name = ANY($1) \
             ORDER BY guild_name, observed_at DESC \
         ) latest",
    )
    .bind(guild_names)
    .bind(season_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load latest observed season ratings: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|(guild_name, season_id, observed_at, season_rating)| {
            (
                guild_name,
                (season_id, observed_at, i64::from(season_rating)),
            )
        })
        .collect())
}

fn apply_snapshot_fallback(
    result: &mut HashMap<String, GuildOnlineEntry>,
    fallback: HashMap<String, (i32, DateTime<Utc>, i64)>,
) {
    for (guild_name, (season_id, observed_at, season_rating)) in fallback {
        let Some(entry) = result.get_mut(&guild_name) else {
            continue;
        };
        if entry.season_rating.is_some() {
            continue;
        }
        entry.season_rating = Some(season_rating);
        entry.season_rank = None;
        entry.season_id = Some(season_id);
        entry.season_rating_source = Some(GuildSeasonRatingSource::SnapshotFallback);
        entry.season_rating_sampled_at = Some(observed_at.to_rfc3339());
    }
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
    let mut url = reqwest::Url::parse(&crate::config::wynncraft_guild_url())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Ok(mut path_segments) = url.path_segments_mut() else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };
    path_segments.push(guild_name);
    drop(path_segments);
    Ok(url)
}

fn cache_guild_payload(state: &AppState, name: String, data: String, observed_at: DateTime<Utc>) {
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
            observed_at,
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

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use chrono::{DateTime, Duration, Utc};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use super::{
        GuildOnlineEntry, GuildSeasonRatingSource, StatusCode, apply_snapshot_fallback,
        guild_details_url, if_none_match_matches, normalize_guild_name, parse_guild_online_entry,
        parse_guilds_online_names, resolve_season_ratings,
    };
    use crate::state::{AppState, CachedSeasonLeaderboard, CachedSeasonLeaderboardEntry};
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
    fn parse_guild_online_entry_keeps_each_seasons_rating_unresolved() {
        let payload = r#"{
            "online": 14,
            "seasonRanks": {
                "29": {"rating": 9000},
                "31": {"rating": 12000},
                "oops": {"rating": 999999}
            }
        }"#;

        let entry =
            parse_guild_online_entry(payload, Utc::now()).expect("guild payload should parse");
        assert_eq!(entry.online, 14);
        assert_eq!(entry.own_ratings, HashMap::from([(29, 9000), (31, 12000)]));
        assert_eq!(
            entry.season_rating, None,
            "no season is current until one is resolved"
        );
    }

    fn own_rating(rating: i64, season_id: i32, observed_at: DateTime<Utc>) -> GuildOnlineEntry {
        let payload =
            format!(r#"{{"online": 3, "seasonRanks": {{"{season_id}": {{"rating": {rating}}}}}}}"#);
        parse_guild_online_entry(&payload, observed_at).expect("guild payload should parse")
    }

    fn leaderboard(
        season_id: i32,
        observed_at: DateTime<Utc>,
        listed: &[(u32, &str, i64)],
    ) -> CachedSeasonLeaderboard {
        CachedSeasonLeaderboard {
            season_id,
            entries: listed
                .iter()
                .map(|&(rank, name, score)| CachedSeasonLeaderboardEntry {
                    rank,
                    name: name.to_string(),
                    uuid: String::new(),
                    prefix: String::new(),
                    score,
                })
                .collect(),
            fetched_at: Utc::now(),
            observed_at,
        }
    }

    #[test]
    fn each_guild_gets_the_later_observation_of_this_seasons_rating() {
        let now = Utc::now();
        let board_seen = now - Duration::seconds(310);
        let mut result = HashMap::from([
            (
                "Sequoia".to_string(),
                own_rating(4_207_221, 33, now - Duration::seconds(32)),
            ),
            (
                "Hesperides".to_string(),
                own_rating(1_579_000, 33, now - Duration::seconds(400)),
            ),
            ("Eden".to_string(), own_rating(21_000_000, 32, now)),
            ("Shades".to_string(), own_rating(90_000, 32, now)),
            (
                "Northbound".to_string(),
                parse_guild_online_entry(r#"{"online": 0}"#, now)
                    .expect("guild payload should parse"),
            ),
        ]);
        let board = leaderboard(
            33,
            board_seen,
            &[
                (1, "Eden", 5_909_449),
                (2, "sequoia", 4_206_533),
                (5, "Hesperides", 1_579_049),
                (12, "Northbound", 579_566),
            ],
        );

        resolve_season_ratings(&mut result, Some(&board), 33);

        let sequoia = &result["Sequoia"];
        assert_eq!(
            sequoia.season_rating,
            Some(4_207_221),
            "observed after the leaderboard"
        );
        assert_eq!(sequoia.season_rank, Some(2));
        assert_eq!(sequoia.season_id, Some(33));
        assert_eq!(
            sequoia.season_rating_source,
            Some(GuildSeasonRatingSource::Live)
        );

        let hesperides = &result["Hesperides"];
        assert_eq!(
            hesperides.season_rating,
            Some(1_579_049),
            "observed before the leaderboard"
        );
        assert_eq!(
            hesperides.season_rating_source,
            Some(GuildSeasonRatingSource::Leaderboard)
        );

        let eden = &result["Eden"];
        assert_eq!(eden.season_rating, Some(5_909_449));
        assert_eq!(eden.season_rank, Some(1));
        assert_eq!(eden.season_id, Some(33));
        assert_eq!(
            eden.season_rating_source,
            Some(GuildSeasonRatingSource::Leaderboard)
        );
        assert_eq!(eden.season_rating_sampled_at, Some(board_seen.to_rfc3339()));

        let shades = &result["Shades"];
        assert_eq!(
            shades.season_rating, None,
            "last season's rating is not this season's"
        );
        assert_eq!(shades.season_id, None);
        assert_eq!(shades.season_rating_source, None);

        let northbound = &result["Northbound"];
        assert_eq!(northbound.season_rating, Some(579_566));
        assert_eq!(
            northbound.season_rating_source,
            Some(GuildSeasonRatingSource::Leaderboard)
        );
    }

    #[test]
    fn a_season_one_guild_has_begun_retires_the_leaderboards_older_one() {
        let now = Utc::now();
        let mut result = HashMap::from([
            ("Cirrus".to_string(), own_rating(1_200, 34, now)),
            ("Eden".to_string(), own_rating(21_000_000, 33, now)),
            ("Shades".to_string(), own_rating(90_000, 33, now)),
        ]);
        let board = leaderboard(33, now, &[(1, "Eden", 21_000_100), (7, "Shades", 90_100)]);

        resolve_season_ratings(&mut result, Some(&board), 34);

        assert_eq!(result["Cirrus"].season_rating, Some(1_200));
        assert_eq!(result["Cirrus"].season_id, Some(34));
        assert_eq!(result["Cirrus"].season_rank, None);
        for name in ["Eden", "Shades"] {
            assert_eq!(
                result[name].season_rating, None,
                "{name} has no season 34 rating yet"
            );
            assert_eq!(result[name].season_rank, None);
            assert_eq!(result[name].season_id, None);
        }
    }

    #[test]
    fn a_guilds_own_season_key_does_not_choose_the_season() {
        let now = Utc::now();
        let mut result = HashMap::from([
            ("Sequoia".to_string(), own_rating(4_207_221, 33, now)),
            ("Bogus".to_string(), own_rating(1, 999, now)),
            (
                "Stray".to_string(),
                parse_guild_online_entry(
                    r#"{"online": 2, "seasonRanks": {"33": {"rating": 5100}, "999": {"rating": 1}}}"#,
                    now,
                )
                .expect("guild payload should parse"),
            ),
        ]);
        let board = leaderboard(
            33,
            now - Duration::seconds(60),
            &[(2, "Sequoia", 4_206_533), (40, "Stray", 5_000)],
        );

        resolve_season_ratings(&mut result, Some(&board), 33);

        assert_eq!(result["Sequoia"].season_rating, Some(4_207_221));
        assert_eq!(result["Bogus"].season_rating, None);
        assert_eq!(result["Bogus"].season_rating_sampled_at, None);
        assert_eq!(
            result["Stray"].season_rating,
            Some(5_100),
            "its season 33 rating still counts"
        );
        assert_eq!(result["Stray"].season_id, Some(33));
    }

    #[test]
    fn apply_snapshot_fallback_backfills_missing_season_rating_only() {
        let observed_at = Utc::now();
        let mut result = HashMap::from([
            (
                "Aequitas".to_string(),
                GuildOnlineEntry {
                    online: 12,
                    season_rating: None,
                    season_rank: None,
                    season_id: None,
                    season_rating_source: None,
                    season_rating_sampled_at: None,
                    own_ratings: HashMap::new(),
                    own_observed_at: observed_at,
                },
            ),
            (
                "Sequoia".to_string(),
                GuildOnlineEntry {
                    online: 8,
                    season_rating: Some(345_000),
                    season_rank: None,
                    season_id: None,
                    season_rating_source: Some(GuildSeasonRatingSource::Live),
                    season_rating_sampled_at: None,
                    own_ratings: HashMap::new(),
                    own_observed_at: observed_at,
                },
            ),
        ]);

        apply_snapshot_fallback(
            &mut result,
            HashMap::from([
                ("Aequitas".to_string(), (33, observed_at, 346_900)),
                ("Sequoia".to_string(), (33, observed_at, 999_999)),
            ]),
        );

        assert_eq!(result["Aequitas"].season_rating, Some(346_900));
        assert_eq!(result["Aequitas"].season_id, Some(33));
        assert_eq!(
            result["Aequitas"].season_rating_source,
            Some(GuildSeasonRatingSource::SnapshotFallback)
        );
        assert_eq!(
            result["Aequitas"].season_rating_sampled_at,
            Some(observed_at.to_rfc3339())
        );
        assert_eq!(result["Sequoia"].season_rating, Some(345_000));
        assert_eq!(
            result["Sequoia"].season_rating_source,
            Some(GuildSeasonRatingSource::Live)
        );
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
    async fn warcontroller_endpoint_refuses_a_viewer_outside_sequoia() {
        use sequoia_shared::WarControllerState;

        let state = AppState::new(None);
        let (addr, server_handle) = spawn_test_server(state.clone()).await;
        let url = format!("http://{addr}/api/warcontroller");
        let client = reqwest::Client::new();

        let payload = r#"{"timestamp": 1787517420, "queues": [], "players": [],
            "wars": [{"territory": "Entrance to Olux", "difficulty": "VERY_HIGH",
                      "health": 0.8731, "start": 1787517417, "ehp": 24135275, "dps": 32143}]}"#;
        let parsed: WarControllerState = serde_json::from_str(payload).expect("payload parses");
        *state.warcontroller_cache.write().await = Some(crate::state::CachedWarController {
            state: parsed,
            json: Arc::new(Bytes::from(payload.as_bytes().to_vec())),
            fetched_at: chrono::Utc::now(),
        });

        // No `seq_session` cookie, so the viewer cannot be a Sequoia member. Internal war
        // intel must be refused outright rather than served or emptied out.
        let response = client
            .get(&url)
            .send()
            .await
            .expect("warcontroller request");
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        let body = response.text().await.expect("warcontroller body");
        assert!(
            !body.contains("Entrance to Olux"),
            "refusal must not leak the feed, got: {body}"
        );

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[test]
    fn an_unconfigured_backend_is_a_verdict_but_an_unreachable_one_is_not() {
        // Two conditions that used to look identical. Both halves share one test, and one
        // runtime, because they disagree about `SEQUOIA_BACKEND_BASE_URL` - running them
        // concurrently would have each see the other's environment.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime");

        // No backend configured at all: nothing could ever resolve this cookie, and no
        // amount of retrying will change that. Answering 503 asked the client to retry a
        // permanent condition forever, so this is an authoritative "signed out" instead -
        // and the war feed is refused on that basis, not held open.
        temp_env::with_var_unset("SEQUOIA_BACKEND_BASE_URL", || {
            runtime.block_on(async {
                let state = AppState::new(None);
                let (addr, server_handle) = spawn_test_server(state).await;
                let client = reqwest::Client::new();

                let response = client
                    .get(format!("http://{addr}/api/warcontroller"))
                    .header(reqwest::header::COOKIE, "seq_session=some.jwt.value")
                    .send()
                    .await
                    .expect("warcontroller request");
                assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

                let response = client
                    .get(format!("http://{addr}/api/auth/me"))
                    .header(reqwest::header::COOKIE, "seq_session=some.jwt.value")
                    .send()
                    .await
                    .expect("me request");
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("me body parses"),
                    serde_json::json!({ "viewer": null, "war_access": false })
                );

                server_handle.abort();
                let _ = server_handle.await;
            });
        });

        // A backend that is configured but cannot be reached - a redeploy, a blip. That is
        // not a verdict on the viewer, so the map must not answer 403: the client reads that
        // as "not a member" and hides the feed until a reload, while a 503 it simply retries.
        let dead_backend = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind dead backend port");
            listener.local_addr().expect("dead backend address")
            // Dropped here, so connecting is refused rather than merely slow.
        });
        temp_env::with_var(
            "SEQUOIA_BACKEND_BASE_URL",
            Some(format!("http://{dead_backend}")),
            || {
                runtime.block_on(async {
                    let state = AppState::new(None);
                    let (addr, server_handle) = spawn_test_server(state).await;
                    let client = reqwest::Client::new();

                    let response = client
                        .get(format!("http://{addr}/api/warcontroller"))
                        .header(reqwest::header::COOKIE, "seq_session=some.jwt.value")
                        .send()
                        .await
                        .expect("warcontroller request");
                    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

                    let response = client
                        .get(format!("http://{addr}/api/auth/me"))
                        .header(reqwest::header::COOKIE, "seq_session=some.jwt.value")
                        .send()
                        .await
                        .expect("me request");
                    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
                    // Never cached, or the next viewer would be handed this failure.
                    assert_eq!(
                        response
                            .headers()
                            .get(reqwest::header::CACHE_CONTROL)
                            .and_then(|value| value.to_str().ok()),
                        Some("private, no-store")
                    );

                    server_handle.abort();
                    let _ = server_handle.await;
                });
            },
        );
    }

    #[tokio::test]
    async fn me_reports_a_signed_out_viewer_when_no_cookie_is_present() {
        // No cookie needs no round trip and is an answer in itself, so this stays a 200 with
        // a null viewer even with no backend configured.
        let state = AppState::new(None);
        let (addr, server_handle) = spawn_test_server(state.clone()).await;

        let response = reqwest::get(format!("http://{addr}/api/auth/me"))
            .await
            .expect("me request");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body = response.text().await.expect("me body");
        assert_eq!(body, r#"{"viewer":null,"war_access":false}"#);

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn sse_stream_withholds_warcontroller_from_a_viewer_outside_sequoia() {
        let state = AppState::new(None);
        let payload = r#"{"timestamp":1787517420,"queues":[],"wars":[],"players":[]}"#;
        *state.warcontroller_cache.write().await = Some(crate::state::CachedWarController {
            state: serde_json::from_str(payload).expect("payload parses"),
            json: Arc::new(Bytes::from(payload.as_bytes().to_vec())),
            fetched_at: chrono::Utc::now(),
        });
        {
            let mut snapshot = state.live_snapshot.write().await;
            snapshot.seq = 7;
            snapshot.snapshot_json = Arc::new(Bytes::from_static(br#"{"territories":{}}"#));
        }

        let (addr, server_handle) = spawn_test_server(state).await;
        let mut response = reqwest::Client::new()
            .get(format!("http://{addr}/api/events"))
            .send()
            .await
            .expect("sse request")
            .error_for_status()
            .expect("sse status");

        // The session probe runs after the snapshot precisely so the map's first paint
        // never waits on it; the snapshot frame is therefore still immediate.
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), response.chunk())
            .await
            .expect("sse frame arrives")
            .expect("sse chunk")
            .expect("sse chunk is present");
        let frame = String::from_utf8_lossy(&chunk).to_string();

        assert!(
            frame.contains("event: snapshot"),
            "expected the snapshot frame, got: {frame}"
        );
        assert!(
            !frame.contains("event: warcontroller"),
            "an anonymous stream must carry no war feed, got: {frame}"
        );
        assert!(
            !frame.contains("1787517420"),
            "an anonymous stream must carry no war payload, got: {frame}"
        );

        server_handle.abort();
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn season_scalar_endpoint_returns_latest_persisted_sample() {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("Skipping season scalar API test: TEST_DATABASE_URL is not set");
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
        crate::test_support::reset_history(&pool).await;

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
