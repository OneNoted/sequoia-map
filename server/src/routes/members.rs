//! A Sequoia member's war standing and record, for other members.
//!
//! The same gate as the war feed ([`super::war_access`]). The player asked about must be on
//! Sequoia's current in-game roster; that membership only decides who can be looked up and
//! says nothing about which guild a past war counted for. The record is the backend's stored points for one UUID,
//! the roles come from the public rank-profile feed. Every answer, refusals included, belongs
//! to the asking session only ([`router`]). Only the public feed is kept here, for a minute,
//! and nothing member-only is stored on this side.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use sequoia_shared::member_war::{
    MemberRosterRoles, MemberStanding, MemberWarPeriod, MemberWarRecord, PeriodKind,
    ProgressionRank, RECENT_WARS, RankStanding, WarRole, WarRoleKind, check_custom_days,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::http_util::mark_private;
use crate::config;
use crate::services::season_data::{self, SeasonWindowSource};
use crate::state::{AppState, CachedRankRoles, RankedProfile};

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// The backend answers connected sessions from memory; a slow answer is not worth holding the
/// roster for.
const CONNECTED_MEMBERS_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest mod version shown; the mod reports its own, unchecked.
const MOD_VERSION_MAX_CHARS: usize = 32;
const PROGRESSION_RANK: &str = "progression_rank";
const ROSTER_RANKS: [&str; 6] = [
    "owner",
    "chief",
    "strategist",
    "captain",
    "recruiter",
    "recruit",
];

/// The current roster entry for a member.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RosterEntry {
    uuid: String,
    username: String,
    rank: &'static str,
}

/// The member routes, with every response marked private at this one boundary.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/members", get(get_roster_roles))
        .route("/api/members/{username}", get(get_member_standing))
        .route("/api/members/{username}/war", get(get_member_war))
        .layer(axum::middleware::map_response(mark_private))
}

/// `GET /api/members`: war roles and progression ranks for the current roster, from one shared
/// read of the feed.
async fn get_roster_roles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let roster = match sequoia_roster(&state, &headers).await {
        Ok(roster) => roster,
        Err(status) => return status.into_response(),
    };
    let (feed, connected) = tokio::join!(rank_roles(&state), connected_members(&state));
    let Some(feed) = feed else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let mut roles = BTreeMap::new();
    let mut progression = BTreeMap::new();
    let mut mod_sessions = connected.as_ref().map(|_| BTreeMap::new());
    for entry in roster_entries(&roster) {
        if let (Some(connected), Some(sessions)) = (&connected, &mut mod_sessions)
            && let Some(versions) = connected.get(&entry.uuid.to_ascii_lowercase())
        {
            sessions.insert(entry.username.clone(), versions.clone());
        }
        if let Some(held) = feed.roles.get(&entry.uuid) {
            roles.insert(entry.username.clone(), held.clone());
        }
        if let Some(rank) = feed.progression.get(&entry.uuid) {
            progression.insert(entry.username, rank.clone());
        }
    }
    json(&MemberRosterRoles {
        // Past its freshness window, the last good read is served only because no newer one
        // could be had; that is stale whatever the feed said at the time.
        stale: feed.stale || !is_fresh(&feed, Utc::now()),
        roles,
        progression,
        mod_sessions,
    })
}

#[derive(Deserialize)]
struct ConnectedMembersBody {
    members: Vec<ConnectedMember>,
}

#[derive(Deserialize)]
struct ConnectedMember {
    minecraft_uuid: String,
    #[serde(default)]
    mod_versions: Vec<String>,
}

/// The backend's guild members with a mod session open now, by lowercase UUID, with the versions
/// worth showing. `None` when the backend could not be asked; asked fresh for every roster read.
async fn connected_members(state: &AppState) -> Option<HashMap<String, Vec<String>>> {
    let mut request = state
        .http_client
        .get(config::connected_members_url()?)
        .timeout(CONNECTED_MEMBERS_TIMEOUT);
    if let Some(token) = config::sequoia_backend_internal_token() {
        request = request.bearer_auth(token);
    }
    let body = request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<ConnectedMembersBody>()
        .await
        .ok()?;
    Some(
        body.members
            .into_iter()
            .map(|member| {
                let mut versions: Vec<String> = member
                    .mod_versions
                    .into_iter()
                    .filter_map(|version| shown_mod_version(&version))
                    .collect();
                versions.sort();
                versions.dedup();
                (member.minecraft_uuid.to_ascii_lowercase(), versions)
            })
            .collect(),
    )
}

/// A reported mod version as shown, or `None` for one that is not a plain version string.
fn shown_mod_version(reported: &str) -> Option<String> {
    let version = reported.trim();
    let plain = !version.is_empty()
        && version.chars().count() <= MOD_VERSION_MAX_CHARS
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        && !version.eq_ignore_ascii_case("unknown");
    plain.then(|| version.to_string())
}

/// `GET /api/members/{username}`: who they are now.
async fn get_member_standing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Response {
    let entry = match member_on_roster(&state, &headers, &username).await {
        Ok(entry) => entry,
        Err(status) => return status.into_response(),
    };
    let ranks = rank_standing(&state, &entry.uuid).await;
    json(&MemberStanding {
        uuid: entry.uuid,
        username: entry.username,
        guild_rank: Some(entry.rank.to_string()),
        ranks,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct PeriodQuery {
    period: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

/// `GET /api/members/{username}/war`: their stored war points over one window.
async fn get_member_war(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Query(query): Query<PeriodQuery>,
) -> Response {
    let entry = match member_on_roster(&state, &headers, &username).await {
        Ok(entry) => entry,
        Err(status) => return status.into_response(),
    };
    let now = Utc::now();
    let window = match resolve_period(&state, &query, now).await {
        Ok(window) => window,
        Err(status) => return status.into_response(),
    };
    let Some(url) = config::war_points_player_url(&entry.uuid) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut request = state
        .http_client
        .get(url)
        .query(&[
            ("from", rfc3339(window.from)),
            ("to", rfc3339(window.to)),
            ("recent", RECENT_WARS.to_string()),
        ])
        .timeout(UPSTREAM_TIMEOUT);
    if let Some(token) = config::sequoia_backend_internal_token() {
        request = request.bearer_auth(token);
    }
    let record = match request.send().await {
        Ok(response) if response.status().is_success() => {
            response.json::<MemberWarRecord>().await.map_err(|error| {
                warn!("member war record was unreadable: {error}");
            })
        }
        Ok(response) => {
            warn!("member war record upstream answered {}", response.status());
            Err(())
        }
        Err(error) => {
            warn!("member war record request failed: {error}");
            Err(())
        }
    };
    match record {
        Ok(record) => json(&MemberWarPeriod {
            kind: window.kind,
            from: rfc3339(window.from),
            to: rfc3339(window.to),
            season_id: window.season_id,
            record,
        }),
        Err(()) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

/// The war gate ([`super::war_access`]), then Sequoia's current roster.
async fn sequoia_roster(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<serde_json::Value, StatusCode> {
    super::war_access::war_access(state, headers)
        .await
        .into_result()?;
    let payload = super::api::guild_payload(
        state,
        config::SEQUOIA_GUILD_NAME.to_string(),
        config::GUILD_CACHE_TTL_SECS,
    )
    .await?;
    serde_json::from_str(&payload).map_err(|_| StatusCode::BAD_GATEWAY)
}

/// [`sequoia_roster`], then the named member on it: `404` for a name not on it.
async fn member_on_roster(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
) -> Result<RosterEntry, StatusCode> {
    let roster = sequoia_roster(state, headers).await?;
    if !is_minecraft_name(username) {
        return Err(StatusCode::BAD_REQUEST);
    }
    find_on_roster(&roster, username).ok_or(StatusCode::NOT_FOUND)
}

fn is_minecraft_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn roster_entries(roster: &serde_json::Value) -> impl Iterator<Item = RosterEntry> + '_ {
    let members = roster.get("members").and_then(serde_json::Value::as_object);
    ROSTER_RANKS.iter().flat_map(move |rank| {
        members
            .and_then(|members| members.get(*rank)?.as_object())
            .into_iter()
            .flatten()
            .filter_map(|(name, info)| {
                Some(RosterEntry {
                    uuid: info.get("uuid")?.as_str()?.to_string(),
                    username: name.clone(),
                    rank,
                })
            })
    })
}

fn find_on_roster(roster: &serde_json::Value, username: &str) -> Option<RosterEntry> {
    roster_entries(roster).find(|entry| entry.username.eq_ignore_ascii_case(username))
}

/// Whether a read of the feed is still within its freshness window.
pub(crate) fn is_fresh(cached: &CachedRankRoles, now: DateTime<Utc>) -> bool {
    (now - cached.fetched_at).num_seconds() < config::RANK_ROLES_CACHE_TTL_SECS
}

/// The linked feed's war roles and progression ranks by UUID, read at most once a minute for every viewer, one read at
/// a time. A failed read keeps serving the last good one, untouched, and the next read waits
/// out a short backoff, so an outage costs one upstream attempt per backoff, not one per viewer.
pub(crate) async fn rank_roles(state: &AppState) -> Option<CachedRankRoles> {
    let fresh = |cached: &&CachedRankRoles| is_fresh(cached, Utc::now());
    if let Some(cached) = state.rank_roles_cache.read().await.as_ref().filter(fresh) {
        return Some(cached.clone());
    }
    let mut last_failure = state.rank_roles_fetch_lock.lock().await;
    if let Some(cached) = state.rank_roles_cache.read().await.as_ref().filter(fresh) {
        return Some(cached.clone());
    }
    let backing_off = last_failure.is_some_and(|failed_at| {
        (Utc::now() - failed_at).num_seconds() < config::RANK_ROLES_FAILURE_BACKOFF_SECS
    });
    if backing_off {
        return state.rank_roles_cache.read().await.clone();
    }
    let fetched = async {
        state
            .http_client
            .get(config::rank_profiles_feed_url())
            .timeout(UPSTREAM_TIMEOUT)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<ProfilesBody>()
            .await
            .ok()
    }
    .await;
    let mut cache = state.rank_roles_cache.write().await;
    match fetched {
        Some(body) => {
            *cache = Some(by_uuid(&body, Utc::now()));
            *last_failure = None;
        }
        None => {
            warn!("linked rank-profile feed could not be read");
            *last_failure = Some(Utc::now());
        }
    }
    cache.clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    kind: PeriodKind,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    season_id: Option<i32>,
}

async fn resolve_period(
    state: &AppState,
    query: &PeriodQuery,
    now: DateTime<Utc>,
) -> Result<Window, StatusCode> {
    match query.period.as_deref().unwrap_or("7d") {
        "7d" => Ok(trailing(PeriodKind::Week, 7, now)),
        "30d" => Ok(trailing(PeriodKind::Month, 30, now)),
        "custom" => custom_window(query.from.as_deref(), query.to.as_deref(), now),
        "season" => {
            let windows = season_data::list_resolved_windows(state)
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            current_season(
                windows.iter().map(|window| {
                    (
                        window.season_id,
                        window.start_at,
                        window.end_at,
                        window.source,
                    )
                }),
                now,
            )
            .ok_or(StatusCode::CONFLICT)
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn trailing(kind: PeriodKind, days: i64, now: DateTime<Utc>) -> Window {
    Window {
        kind,
        from: now - chrono::Duration::days(days),
        to: now,
        season_id: None,
    }
}

/// The season running now, from a configured or Wynncraft-published window. An inferred window
/// is a guess from observations, so it never bounds a member's record.
fn current_season(
    windows: impl Iterator<Item = (i32, DateTime<Utc>, DateTime<Utc>, SeasonWindowSource)>,
    now: DateTime<Utc>,
) -> Option<Window> {
    windows
        .filter(|(_, start, end, source)| {
            *source != SeasonWindowSource::Inferred && *start <= now && now < *end
        })
        .max_by_key(|(season_id, ..)| *season_id)
        .map(|(season_id, start, ..)| Window {
            kind: PeriodKind::Season,
            from: start,
            to: now,
            season_id: Some(season_id),
        })
}

/// Whole UTC days, both inclusive, ending no later than today.
fn custom_window(
    from: Option<&str>,
    to: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Window, StatusCode> {
    let day = |value: Option<&str>| {
        value
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
            .ok_or(StatusCode::BAD_REQUEST)
    };
    let (first, last) = (day(from)?, day(to)?);
    let today = now.date_naive();
    check_custom_days(first, last, today).map_err(|_| StatusCode::BAD_REQUEST)?;
    let start = first
        .and_hms_opt(0, 0, 0)
        .ok_or(StatusCode::BAD_REQUEST)?
        .and_utc();
    let end = if last == today {
        now
    } else {
        last.succ_opt()
            .and_then(|next| next.and_hms_opt(0, 0, 0))
            .ok_or(StatusCode::BAD_REQUEST)?
            .and_utc()
    };
    Ok(Window {
        kind: PeriodKind::Custom,
        from: start,
        to: end,
        season_id: None,
    })
}

/// What the roster rows and the war gate need from one read of the whole feed, by Minecraft
/// UUID: war roles and progression ranks for the profiles that hold any, and where those ranks
/// sit in the hierarchy.
fn by_uuid(body: &ProfilesBody, fetched_at: DateTime<Utc>) -> CachedRankRoles {
    let (mut roles, mut progression, mut ranked) = (HashMap::new(), HashMap::new(), HashMap::new());
    for profile in &body.profiles {
        let Some(minecraft) = profile.minecraft.as_ref() else {
            continue;
        };
        let held = profile_war_roles(&body.catalog, profile);
        if !held.is_empty() {
            roles.insert(minecraft.uuid.clone(), held);
        }
        if let Some(role) = progression_role(&body.catalog, profile) {
            if let Some(position) = role.position {
                let discord_id = profile
                    .discord
                    .as_ref()
                    .and_then(|discord| discord.id.clone());
                ranked.insert(
                    minecraft.uuid.clone(),
                    RankedProfile {
                        discord_id,
                        position,
                    },
                );
            }
            progression.insert(minecraft.uuid.clone(), progression_rank_of(role));
        }
    }
    let rank_positions = body
        .catalog
        .roles
        .iter()
        .filter(|role| role.category == PROGRESSION_RANK)
        .filter_map(|role| Some((role.key.clone(), role.position?)))
        .collect();
    let parse = |time: &str| {
        DateTime::parse_from_rfc3339(time)
            .ok()
            .map(|time| time.with_timezone(&Utc))
    };
    let synced_at = body
        .source
        .guilds
        .iter()
        .map(|guild| guild.last_full_sync_at.as_deref().and_then(parse))
        .try_fold(None, |oldest: Option<DateTime<Utc>>, synced| {
            let synced = synced?;
            Some(Some(oldest.map_or(synced, |oldest| oldest.min(synced))))
        })
        .flatten();
    CachedRankRoles {
        stale: body.source.stale,
        roles,
        progression,
        ranked,
        rank_positions,
        generated_at: body.generated_at.as_deref().and_then(parse),
        synced_at,
        fetched_at,
    }
}

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn rank_standing(state: &AppState, uuid: &str) -> RankStanding {
    let response = state
        .http_client
        .get(config::rank_profile_url(uuid))
        .timeout(UPSTREAM_TIMEOUT)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!("rank profile request failed: {error}");
            return RankStanding::Unavailable;
        }
    };
    match response.status() {
        StatusCode::NOT_FOUND => RankStanding::NoProfile,
        status if status.is_success() => match response.json::<ProfilesBody>().await {
            Ok(body) => war_roles(body),
            Err(error) => {
                warn!("rank profile was unreadable: {error}");
                RankStanding::Unavailable
            }
        },
        status => {
            warn!("rank profile upstream answered {status}");
            RankStanding::Unavailable
        }
    }
}

/// The published rank-profile shape, as far as war roles and progression ranks need it.
#[derive(Debug, Deserialize)]
struct ProfilesBody {
    #[serde(default)]
    generated_at: Option<String>,
    source: ProfileSource,
    catalog: ProfileCatalog,
    profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize)]
struct ProfileSource {
    stale: bool,
    #[serde(default)]
    guilds: Vec<SourceGuild>,
}

#[derive(Debug, Deserialize)]
struct SourceGuild {
    #[serde(default)]
    last_full_sync_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfileCatalog {
    roles: Vec<CatalogRole>,
    #[serde(default)]
    assets: Vec<CatalogAsset>,
}

#[derive(Debug, Deserialize)]
struct CatalogRole {
    key: String,
    label: String,
    category: String,
    #[serde(default)]
    asset_key: Option<String>,
    #[serde(default)]
    colors: Option<RoleColors>,
    /// The role's Discord position: higher sits above. Absent until the backend has synced it.
    #[serde(default)]
    position: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RoleColors {
    #[serde(default)]
    primary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogAsset {
    key: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct Profile {
    #[serde(default)]
    discord: Option<ProfileDiscord>,
    #[serde(default)]
    minecraft: Option<ProfileMinecraft>,
    role_keys: Vec<String>,
    summary: ProfileSummary,
}

#[derive(Debug, Deserialize)]
struct ProfileDiscord {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfileMinecraft {
    uuid: String,
}

#[derive(Debug, Deserialize)]
struct ProfileSummary {
    #[serde(default)]
    progression_rank: Option<String>,
    #[serde(default)]
    insignia: Option<String>,
    #[serde(default)]
    war_honour: Option<String>,
}

/// One profile's standing: synced with its war roles and progression rank, or no profile at all.
fn war_roles(body: ProfilesBody) -> RankStanding {
    match body.profiles.first() {
        Some(profile) => RankStanding::Synced {
            stale: body.source.stale,
            roles: profile_war_roles(&body.catalog, profile),
            progression: progression_role(&body.catalog, profile).map(progression_rank_of),
        },
        None => RankStanding::NoProfile,
    }
}

/// The catalog role for the progression rank the profile's summary names, when the catalog lists
/// that key as one.
fn progression_role<'a>(catalog: &'a ProfileCatalog, profile: &Profile) -> Option<&'a CatalogRole> {
    let key = profile.summary.progression_rank.as_deref()?;
    catalog
        .roles
        .iter()
        .find(|role| role.key == key && role.category == PROGRESSION_RANK)
}

fn progression_rank_of(role: &CatalogRole) -> ProgressionRank {
    ProgressionRank {
        key: role.key.clone(),
        label: role.label.clone(),
        color: role
            .colors
            .as_ref()
            .and_then(|colors| colors.primary.clone()),
    }
}

/// The war roles a profile holds, classified by the feed's own catalog: the highest insignia and
/// War Honour the feed summarises, and every military and specialty role.
fn profile_war_roles(catalog: &ProfileCatalog, profile: &Profile) -> Vec<WarRole> {
    let role = |key: &str, kind: WarRoleKind| {
        let role = catalog.roles.iter().find(|role| role.key == key)?;
        let art_url = role.asset_key.as_ref().and_then(|asset_key| {
            catalog
                .assets
                .iter()
                .find(|asset| &asset.key == asset_key)
                .map(|asset| asset.url.clone())
        });
        Some(WarRole {
            key: role.key.clone(),
            label: role.label.clone(),
            kind,
            color: role
                .colors
                .as_ref()
                .and_then(|colors| colors.primary.clone()),
            art_url,
        })
    };
    let mut roles: Vec<WarRole> = [
        (profile.summary.insignia.as_deref(), WarRoleKind::Insignia),
        (
            profile.summary.war_honour.as_deref(),
            WarRoleKind::WarHonour,
        ),
    ]
    .into_iter()
    .filter_map(|(key, kind)| role(key?, kind))
    .collect();
    roles.extend(profile.role_keys.iter().filter_map(|key| {
        let category = catalog
            .roles
            .iter()
            .find(|role| &role.key == key)?
            .category
            .as_str();
        match category {
            "military" => role(key, WarRoleKind::Military),
            "military_specialty" => role(key, WarRoleKind::MilitarySpecialty),
            _ => None,
        }
    }));
    roles.sort_by_key(|role| role.kind);
    roles
}

fn json(body: &impl Serialize) -> Response {
    axum::Json(body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(rfc: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc)
            .expect("time")
            .with_timezone(&Utc)
    }

    /// A stand-in backend: `/auth/web/me` answers for the bearer `member` or `outsider`, a current
    /// rank feed places the member's Discord account at Sprite, and the war-points route always
    /// fails, as an outage would.
    async fn spawn_backend() -> std::net::SocketAddr {
        async fn me(headers: HeaderMap) -> Response {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let rank = match bearer {
                "Bearer member" => serde_json::json!("chief"),
                "Bearer outsider" => serde_json::Value::Null,
                _ => return StatusCode::UNAUTHORIZED.into_response(),
            };
            axum::Json(serde_json::json!({
                "discord_id": "1",
                "minecraft_uuid": "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b",
                "guild_rank": rank,
            }))
            .into_response()
        }
        let feed = || {
            let now = Utc::now().to_rfc3339();
            axum::Json(serde_json::json!({
                "generated_at": now,
                "source": {"stale": false, "guilds": [{"guild_id": "1", "last_full_sync_at": now}]},
                "catalog": {"roles": [{"key": "rank.sprite", "label": "Sprite",
                                       "category": "progression_rank", "position": 90}]},
                "profiles": [{"discord": {"id": "1"},
                              "minecraft": {"uuid": "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b"},
                              "role_keys": ["rank.sprite"],
                              "summary": {"progression_rank": "rank.sprite"}}]
            }))
        };
        let backend = Router::new()
            .route("/auth/web/me", get(me))
            .route("/v1/rank-profiles", get(move || async move { feed() }))
            .route(
                "/internal/war-points/players/{uuid}",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route(
                "/internal/connected-members",
                get(|| async {
                    axum::Json(serde_json::json!({"members": [
                        {"minecraft_uuid": "EE860B7C-9A1D-49CF-9F19-AB673BA0F23B",
                         "minecraft_username": "epicpuppy613",
                         "mod_versions": ["1.10.14", "unknown", "<b>1.0</b>", "1.10.14"]},
                        {"minecraft_uuid": "22222222-2222-2222-2222-222222222222",
                         "minecraft_username": "NotInTheGuildNow", "mod_versions": ["1.10.14"]}
                    ]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move { axum::serve(listener, backend).await.expect("serve") });
        addr
    }

    #[test]
    fn a_failing_rank_feed_is_asked_once_per_backoff_and_keeps_its_last_read() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let asked = Arc::new(AtomicUsize::new(0));
        let up = Arc::new(AtomicBool::new(false));
        let feed = runtime.block_on({
            let (asked, up) = (Arc::clone(&asked), Arc::clone(&up));
            async move {
                let profiles = get(move || {
                    let (asked, up) = (Arc::clone(&asked), Arc::clone(&up));
                    async move {
                        asked.fetch_add(1, Ordering::SeqCst);
                        if !up.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        axum::Json(serde_json::json!({
                            "source": {"stale": false},
                            "catalog": {"roles": [], "assets": []},
                            "profiles": []
                        }))
                        .into_response()
                    }
                });
                let app = Router::new().route("/v1/rank-profiles", profiles);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind");
                let addr = listener.local_addr().expect("address");
                tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
                addr
            }
        });
        temp_env::with_var(
            "SEQUOIA_RANK_PROFILES_BASE_URL",
            Some(format!("http://{feed}")),
            || {
                runtime.block_on(async {
                    let state = AppState::new(None);
                    let read_at = Utc::now() - chrono::Duration::seconds(120);
                    let last_good = CachedRankRoles {
                        stale: false,
                        roles: [("uuid".to_string(), Vec::new())].into(),
                        progression: HashMap::new(),
                        ranked: HashMap::new(),
                        rank_positions: HashMap::new(),
                        generated_at: None,
                        synced_at: None,
                        fetched_at: read_at,
                    };
                    *state.rank_roles_cache.write().await = Some(last_good);

                    let reads = futures::future::join_all((0..8).map(|_| rank_roles(&state))).await;
                    assert_eq!(
                        asked.load(Ordering::SeqCst),
                        1,
                        "one attempt for eight viewers"
                    );
                    for read in &reads {
                        let read = read.as_ref().expect("the last good read is served");
                        assert_eq!(read.fetched_at, read_at, "a failure does not renew its age");
                        assert!(!read.stale, "the feed's own flag is kept as it was");
                        assert!(
                            !is_fresh(read, Utc::now()),
                            "and it is served as past fresh"
                        );
                    }
                    rank_roles(&state).await;
                    assert_eq!(
                        asked.load(Ordering::SeqCst),
                        1,
                        "no attempt inside the backoff"
                    );

                    up.store(true, Ordering::SeqCst);
                    let backoff =
                        chrono::Duration::seconds(config::RANK_ROLES_FAILURE_BACKOFF_SECS + 1);
                    *state.rank_roles_fetch_lock.lock().await = Some(Utc::now() - backoff);
                    let recovered = rank_roles(&state).await.expect("a fresh read");
                    assert_eq!(
                        asked.load(Ordering::SeqCst),
                        2,
                        "one attempt once the backoff ends"
                    );
                    assert!(recovered.fetched_at > read_at && is_fresh(&recovered, Utc::now()));
                    assert_eq!(*state.rank_roles_fetch_lock.lock().await, None);
                });
            },
        );
    }

    #[test]
    fn every_member_route_answer_is_private_to_the_asking_session() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let backend = format!("http://{}", runtime.block_on(spawn_backend()));
        temp_env::with_vars(
            [
                ("SEQUOIA_BACKEND_BASE_URL", Some(backend.as_str())),
                ("SEQUOIA_RANK_PROFILES_BASE_URL", Some(backend.as_str())),
            ],
            || {
                runtime.block_on(async {
                let state = AppState::new(None);
                state.guild_cache.insert(
                    config::SEQUOIA_GUILD_NAME.to_string(),
                    crate::state::CachedGuild {
                        data: r#"{"members":{"chief":{"EpicPuppy613":{"uuid":"ee860b7c-9a1d-49cf-9f19-ab673ba0f23b"}}}}"#
                            .to_string(),
                        fetched_at: Utc::now(),
                    },
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
                let addr = listener.local_addr().expect("address");
                let app = crate::app::build_app(state);
                tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
                let client = reqwest::Client::new();
                let cases = [
                    (None, "/api/members", 403),
                    (None, "/api/members/EpicPuppy613", 403),
                    (Some("outsider"), "/api/members/EpicPuppy613/war", 403),
                    (Some("member"), "/api/members/NotOnTheRoster", 404),
                    (Some("member"), "/api/members/bad%20name", 400),
                    (Some("member"), "/api/members/EpicPuppy613/war?period=forever", 400),
                    (Some("member"), "/api/members/EpicPuppy613/war?period=7d", 502),
                ];
                for (session, path, expected) in cases {
                    let mut request = client.get(format!("http://{addr}{path}"));
                    if let Some(session) = session {
                        request = request.header("cookie", format!("seq_session={session}"));
                    }
                    let response = request.send().await.expect("request");
                    assert_eq!(response.status().as_u16(), expected, "{path}");
                    let header = |name| {
                        response
                            .headers()
                            .get(name)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string)
                    };
                    assert_eq!(header("cache-control").as_deref(), Some("private, no-store"), "{path}");
                    assert_eq!(header("vary").as_deref(), Some("Cookie"), "{path}");
                }
                let roster: MemberRosterRoles = client
                    .get(format!("http://{addr}/api/members"))
                    .header("cookie", "seq_session=member")
                    .send()
                    .await
                    .expect("request")
                    .json()
                    .await
                    .expect("roster");
                assert_eq!(
                    roster.mod_sessions,
                    Some(BTreeMap::from([(
                        "EpicPuppy613".to_string(),
                        vec!["1.10.14".to_string()]
                    )])),
                    "matched by UUID under the roster's own spelling, with only plain versions"
                );
            });
            },
        );
    }

    #[test]
    fn only_plain_mod_versions_are_shown() {
        assert_eq!(shown_mod_version(" 1.10.14 ").as_deref(), Some("1.10.14"));
        assert_eq!(
            shown_mod_version("1.11.0-beta.2+fabric").as_deref(),
            Some("1.11.0-beta.2+fabric")
        );
        for hidden in ["", "unknown", "UNKNOWN", "1.0 <b>", &"9".repeat(33)] {
            assert_eq!(shown_mod_version(hidden), None, "{hidden:?}");
        }
    }

    #[test]
    fn a_roster_lookup_finds_the_member_case_insensitively_with_their_current_rank() {
        let roster = serde_json::json!({
            "members": {
                "total": 2,
                "chief": {"EpicPuppy613": {"uuid": "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b", "online": true}},
                "recruit": {"Newbie": {"uuid": "11111111-1111-1111-1111-111111111111", "online": false}}
            }
        });
        assert_eq!(
            find_on_roster(&roster, "epicpuppy613"),
            Some(RosterEntry {
                uuid: "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b".into(),
                username: "EpicPuppy613".into(),
                rank: "chief",
            })
        );
        assert_eq!(find_on_roster(&roster, "Outsider"), None);
        assert!(!is_minecraft_name("../etc"));
        assert!(!is_minecraft_name("seventeen_chars__"));
    }

    #[test]
    fn only_a_published_season_running_now_bounds_a_season_record() {
        let now = at("2026-11-01T12:00:00Z");
        let windows = [
            (
                30,
                at("2026-09-01T00:00:00Z"),
                at("2026-10-10T00:00:00Z"),
                SeasonWindowSource::WynncraftApi,
            ),
            (
                31,
                at("2026-10-15T00:00:00Z"),
                at("2026-12-01T00:00:00Z"),
                SeasonWindowSource::Configured,
            ),
        ];
        let season = current_season(windows.into_iter(), now).expect("season 31 is running");
        assert_eq!(
            (season.season_id, season.from, season.to),
            (Some(31), windows[1].1, now)
        );

        let guessed = [(31, windows[1].1, windows[1].2, SeasonWindowSource::Inferred)];
        assert_eq!(current_season(guessed.into_iter(), now), None);
        assert_eq!(current_season(windows[..1].iter().copied(), now), None);
    }

    #[test]
    fn custom_days_are_whole_utc_days_ending_now_at_the_latest() {
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        let past = custom_window(Some("2026-10-01"), Some("2026-10-07"), now).expect("valid");
        assert_eq!(
            (past.from, past.to),
            (at("2026-10-01T00:00:00Z"), at("2026-10-08T00:00:00Z"))
        );
        let today = custom_window(Some("2026-11-01"), Some("2026-11-01"), now).expect("valid");
        assert_eq!(today.to, now);
        // 400 days, both ends included, is the longest window; 401 is refused.
        let longest = custom_window(Some("2025-09-28"), Some("2026-11-01"), now).expect("400 days");
        assert_eq!(longest.from, at("2025-09-28T00:00:00Z"));
        for (from, to) in [
            ("2025-09-27", "2026-11-01"),
            ("2026-10-07", "2026-10-01"),
            ("2026-10-01", "2026-11-02"),
            ("2025-01-01", "2026-10-01"),
            ("yesterday", "2026-10-01"),
        ] {
            assert_eq!(
                custom_window(Some(from), Some(to), now),
                Err(StatusCode::BAD_REQUEST),
                "{from}..{to}"
            );
        }
    }

    #[test]
    fn war_roles_come_from_the_catalog_with_insignia_art_and_no_unrelated_roles() {
        let body: ProfilesBody = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "source": {"stale": true, "guilds": []},
            "catalog": {
                "roles": [
                    {"key": "insignia.gold", "label": "Gold Insignia", "category": "insignia",
                     "asset_key": "insignia.gold", "colors": {"primary": "#F5C542"}},
                    {"key": "war_honour.silver", "label": "Silver Honours", "category": "war_honour"},
                    {"key": "military.member", "label": "Military", "category": "military"},
                    {"key": "military.build.healer", "label": "War Healer", "category": "military_specialty"},
                    {"key": "notification.defender", "label": "Defender", "category": "notification"}
                ],
                "assets": [{"key": "insignia.gold", "url": "https://assets.example/v1/abc/insignia.gold.png"}],
                "awards": []
            },
            "profiles": [{
                "role_keys": ["military.build.healer", "military.member", "notification.defender",
                              "insignia.gold", "war_honour.silver"],
                "award_keys": [],
                "summary": {"insignia": "insignia.gold", "war_honour": "war_honour.silver"}
            }]
        }))
        .expect("published shape");
        let RankStanding::Synced {
            stale,
            roles,
            progression,
        } = war_roles(body)
        else {
            panic!("a profile is synced");
        };
        assert!(stale);
        assert_eq!(progression, None, "no progression rank is made up");
        let shown: Vec<(&str, WarRoleKind)> = roles
            .iter()
            .map(|role| (role.key.as_str(), role.kind))
            .collect();
        assert_eq!(
            shown,
            [
                ("insignia.gold", WarRoleKind::Insignia),
                ("war_honour.silver", WarRoleKind::WarHonour),
                ("military.member", WarRoleKind::Military),
                ("military.build.healer", WarRoleKind::MilitarySpecialty),
            ]
        );
        assert_eq!(
            roles[0].art_url.as_deref(),
            Some("https://assets.example/v1/abc/insignia.gold.png")
        );
        assert_eq!(roles[1].art_url, None);
    }

    #[test]
    fn progression_ranks_are_the_summary_key_joined_to_the_catalog_and_nothing_else() {
        let body: ProfilesBody = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "source": {"stale": false, "guilds": []},
            "catalog": {
                "roles": [
                    {"key": "rank.ascended", "label": "Ascended", "category": "progression_rank",
                     "colors": {"primary": "#520695", "secondary": "#9A012F"}},
                    {"key": "rank.upper_strategist", "label": "Upper Strategist",
                     "category": "progression_rank", "colors": {"primary": null}},
                    {"key": "in_game.chief", "label": "Chief", "category": "in_game_rank"}
                ],
                "assets": []
            },
            "profiles": [
                {"minecraft": {"uuid": "66efb975-31b4-499e-9b46-a34980edd8ee", "username": "GaztheCat"},
                 "discord": {"display_name": "Ascended GaztheCat"},
                 "role_keys": ["rank.ascended", "in_game.chief"],
                 "summary": {"progression_rank": "rank.ascended", "in_game_rank": "in_game.chief"}},
                {"minecraft": {"uuid": "uncoloured"}, "role_keys": [],
                 "summary": {"progression_rank": "rank.upper_strategist"}},
                {"minecraft": {"uuid": "nick-only"}, "discord": {"display_name": "Yggdrasil Someone"},
                 "role_keys": [], "summary": {}},
                {"minecraft": {"uuid": "not-a-title"}, "role_keys": [],
                 "summary": {"progression_rank": "in_game.chief"}},
                {"minecraft": {"uuid": "unknown-key"}, "role_keys": [],
                 "summary": {"progression_rank": "rank.retired"}},
                {"role_keys": [], "summary": {"progression_rank": "rank.ascended"}}
            ]
        }))
        .expect("published shape");
        let read = by_uuid(&body, at("2026-11-01T12:00:00Z"));
        assert_eq!(
            read.progression.get("66efb975-31b4-499e-9b46-a34980edd8ee"),
            Some(&ProgressionRank {
                key: "rank.ascended".into(),
                label: "Ascended".into(),
                color: Some("#520695".into()),
            }),
            "held even with no war roles"
        );
        assert_eq!(read.progression["uncoloured"].color, None);
        assert_eq!(read.progression.len(), 2, "{:?}", read.progression);
        assert!(read.roles.is_empty());

        let RankStanding::Synced { progression, .. } = war_roles(body) else {
            panic!("a profile is synced");
        };
        assert_eq!(
            progression.map(|rank| rank.label).as_deref(),
            Some("Ascended")
        );
    }
}
