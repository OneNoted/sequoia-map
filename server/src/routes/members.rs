//! A Sequoia member's war standing and record, for other members.
//!
//! The same member gate as the war feed. The player asked about must be on Sequoia's current
//! in-game roster; that membership only decides who can be looked up and says nothing about
//! which guild a past war counted for. The record is the backend's stored points for one UUID,
//! the roles come from the public rank-profile feed. Every answer, refusals included, belongs
//! to the asking session only ([`router`]). Nothing is cached or stored on this side.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use sequoia_shared::member_war::{
    MAX_CUSTOM_DAYS, MemberStanding, MemberWarPeriod, MemberWarRecord, PeriodKind, RECENT_WARS,
    RankStanding, WarRole, WarRoleKind,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::http_util::mark_private;
use crate::config;
use crate::services::season_data::{self, SeasonWindowSource};
use crate::state::AppState;

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
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
        .route("/api/members/{username}", get(get_member_standing))
        .route("/api/members/{username}/war", get(get_member_war))
        .layer(axum::middleware::map_response(mark_private))
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

/// The gate, then the roster: `403` for anyone but a member, `404` for a name not on it.
async fn member_on_roster(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
) -> Result<RosterEntry, StatusCode> {
    let probe = super::auth::resolve_viewer(state, headers).await;
    if probe.is_unavailable() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if !super::auth::viewer_is_guild_member(probe.viewer()) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !is_minecraft_name(username) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let payload = super::api::guild_payload(state, config::SEQUOIA_GUILD_NAME.to_string()).await?;
    let roster: serde_json::Value =
        serde_json::from_str(&payload).map_err(|_| StatusCode::BAD_GATEWAY)?;
    find_on_roster(&roster, username).ok_or(StatusCode::NOT_FOUND)
}

fn is_minecraft_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn find_on_roster(roster: &serde_json::Value, username: &str) -> Option<RosterEntry> {
    let members = roster.get("members")?.as_object()?;
    ROSTER_RANKS.iter().find_map(|rank| {
        let (name, info) = members
            .get(*rank)?
            .as_object()?
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(username))?;
        Some(RosterEntry {
            uuid: info.get("uuid")?.as_str()?.to_string(),
            username: name.clone(),
            rank,
        })
    })
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
    if first > last || last > today || (last - first).num_days() >= MAX_CUSTOM_DAYS {
        return Err(StatusCode::BAD_REQUEST);
    }
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

/// The published rank-profile shape, as far as war roles need it.
#[derive(Debug, Deserialize)]
struct ProfilesBody {
    source: ProfileSource,
    catalog: ProfileCatalog,
    profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize)]
struct ProfileSource {
    stale: bool,
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
    role_keys: Vec<String>,
    summary: ProfileSummary,
}

#[derive(Debug, Deserialize)]
struct ProfileSummary {
    #[serde(default)]
    insignia: Option<String>,
    #[serde(default)]
    war_honour: Option<String>,
}

/// The war roles a profile holds, classified by the feed's own catalog: the highest insignia and
/// War Honour the feed summarises, and every military and specialty role.
fn war_roles(body: ProfilesBody) -> RankStanding {
    let Some(profile) = body.profiles.into_iter().next() else {
        return RankStanding::NoProfile;
    };
    let catalog = &body.catalog;
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
    RankStanding::Synced {
        stale: body.source.stale,
        roles,
    }
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

    /// A stand-in backend: `/auth/web/me` answers for the bearer `member` or `outsider`, and the
    /// war-points route always fails, as an outage would.
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
        let backend = Router::new().route("/auth/web/me", get(me)).route(
            "/internal/war-points/players/{uuid}",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move { axum::serve(listener, backend).await.expect("serve") });
        addr
    }

    #[test]
    fn every_member_route_answer_is_private_to_the_asking_session() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let backend = runtime.block_on(spawn_backend());
        temp_env::with_var(
            "SEQUOIA_BACKEND_BASE_URL",
            Some(format!("http://{backend}")),
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
            });
            },
        );
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
        for (from, to) in [
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
        let RankStanding::Synced { stale, roles } = war_roles(body) else {
            panic!("a profile is synced");
        };
        assert!(stale);
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
}
