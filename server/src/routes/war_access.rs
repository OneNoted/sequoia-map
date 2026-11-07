//! Who may see the map's guild-internal war features: current Sequoia members whose synced
//! progression rank is Sprite or above, and website administrators.
//!
//! Membership is the session probe's in-game guild rank. The progression rank is a different
//! thing - a Discord role the backend syncs into its public rank-profile feed - so it comes from
//! that feed, ordered by the feed's own role positions rather than a list kept here.

use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use super::auth::{self, Viewer, ViewerProbe};
use crate::config::WAR_RANK_FLOOR;
use crate::state::{AppState, CachedRankRoles};

/// How long one session's answer is reused, so bursts of member lookups and every open stream's
/// revalidation cost the backend at most one session lookup per session per window.
const DECISION_TTL: Duration = Duration::from_secs(30);
/// Answers kept before expired ones are swept.
const MAX_DECISIONS: usize = 4096;
/// The oldest feed snapshot that may decide, by the backend's own `generated_at`. It rebuilds
/// every 15 s and is cached for 30 s; on a failed rebuild it keeps serving the old snapshot,
/// stamp and staleness flag included, so only this age can tell a frozen feed from a live one.
const RANK_SNAPSHOT_MAX_AGE_SECS: i64 = 300;
/// The oldest last full Discord sync that may decide. The backend resyncs every 30 minutes and
/// flags its own snapshot stale past that; this holds the same line however old the flag is.
const RANK_SYNC_MAX_AGE_SECS: i64 = 35 * 60;
/// How far ahead of this server's clock a backend stamp may be before it is not believed.
const CLOCK_SKEW_SECS: i64 = 60;

/// The answer for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarAccess {
    Granted,
    /// An authoritative no: signed out, a session the backend refused, outside Sequoia, or below
    /// the floor - which includes a member the feed holds no positioned rank for.
    Denied,
    /// The session or the rank could not be read freshly. Says nothing about the viewer, and is
    /// never remembered.
    Unavailable,
}

impl WarAccess {
    /// `Ok` when granted, otherwise the status to refuse with: `403` for a denial and `503`
    /// when the answer could not be had, so a client can tell a verdict from an outage.
    pub fn into_result(self) -> Result<(), StatusCode> {
        match self {
            Self::Granted => Ok(()),
            Self::Denied => Err(StatusCode::FORBIDDEN),
            Self::Unavailable => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    /// The answer as the client is told it: `None` while it cannot be had.
    pub fn known(self) -> Option<bool> {
        match self {
            Self::Granted => Some(true),
            Self::Denied => Some(false),
            Self::Unavailable => None,
        }
    }
}

/// Decides whether the request's session may see war features. Signed-out requests cost no
/// upstream call; other answers except an outage are remembered per session for
/// [`DECISION_TTL`].
pub async fn war_access(state: &AppState, headers: &HeaderMap) -> WarAccess {
    let Some(token) = auth::session_cookie(headers) else {
        return WarAccess::Denied;
    };
    let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if let Some(remembered) = state.war_access.get(&key)
        && remembered.0 > Instant::now()
    {
        return remembered.1;
    }
    let access = match auth::fetch_viewer(state, token).await {
        ViewerProbe::Viewer(viewer) => viewer_war_access(state, &viewer).await,
        ViewerProbe::Anonymous => WarAccess::Denied,
        ViewerProbe::Unavailable => WarAccess::Unavailable,
    };
    if access != WarAccess::Unavailable {
        remember(state, key, access);
    }
    access
}

/// The answer for an already verified viewer. Admins and outsiders need no rank feed.
pub async fn viewer_war_access(state: &AppState, viewer: &Viewer) -> WarAccess {
    if viewer.website_admin {
        return WarAccess::Granted;
    }
    if viewer.guild_rank.is_none() {
        return WarAccess::Denied;
    }
    let ranks = super::members::rank_roles(state).await;
    rank_decision(viewer, ranks.as_ref(), Utc::now())
}

/// A member's answer from the rank feed.
///
/// Only current evidence decides, and only for the account it belongs to. Anything else is
/// unavailable rather than a verdict: a read past this server's minute (the roster keeps
/// showing a failing feed's last good one), a snapshot the feed flags stale, one whose own
/// stamps are old or missing, a catalog that cannot place the floor, or a rank the feed links
/// to another Discord account than the session's.
fn rank_decision(
    viewer: &Viewer,
    ranks: Option<&CachedRankRoles>,
    now: DateTime<Utc>,
) -> WarAccess {
    let Some(ranks) = ranks.filter(|ranks| evidence_is_current(ranks, now)) else {
        return WarAccess::Unavailable;
    };
    let uuid = viewer.minecraft_uuid.to_ascii_lowercase();
    match ranks.progression_at_least(&uuid, &viewer.discord_id, WAR_RANK_FLOOR) {
        Some(true) => WarAccess::Granted,
        Some(false) => WarAccess::Denied,
        None => WarAccess::Unavailable,
    }
}

/// Whether a feed read is current by every clock it carries: this server's receipt, the
/// backend's snapshot stamp and the Discord syncs behind it.
fn evidence_is_current(ranks: &CachedRankRoles, now: DateTime<Utc>) -> bool {
    let within = |time: Option<DateTime<Utc>>, max_age: i64| {
        time.is_some_and(|time| (-CLOCK_SKEW_SECS..=max_age).contains(&(now - time).num_seconds()))
    };
    !ranks.stale
        && super::members::is_fresh(ranks, now)
        && within(ranks.generated_at, RANK_SNAPSHOT_MAX_AGE_SECS)
        && within(ranks.synced_at, RANK_SYNC_MAX_AGE_SECS)
}

fn remember(state: &AppState, key: [u8; 32], access: WarAccess) {
    let now = Instant::now();
    if state.war_access.len() >= MAX_DECISIONS {
        state.war_access.retain(|_, (expires, _)| *expires > now);
    }
    state.war_access.insert(key, (now + DECISION_TTL, access));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};

    use axum::Router;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;

    const MEMBER: &str = "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b";

    fn member() -> Viewer {
        Viewer {
            discord_id: "1".to_string(),
            discord_username: None,
            minecraft_uuid: MEMBER.to_string(),
            minecraft_username: None,
            website_admin: false,
            guild_rank: Some("recruit".to_string()),
        }
    }

    /// A healthy read at `now`: the live catalog's positions around the floor, a snapshot built
    /// seconds ago and Discord synced ten minutes ago, with `held` linked to [`member`]'s
    /// Discord account.
    fn feed(held: Option<i64>, now: DateTime<Utc>) -> CachedRankRoles {
        CachedRankRoles {
            stale: false,
            roles: HashMap::new(),
            progression: HashMap::new(),
            ranked: held
                .map(|position| {
                    let discord_id = Some("1".to_string());
                    (
                        MEMBER.to_string(),
                        crate::state::RankedProfile {
                            discord_id,
                            position,
                        },
                    )
                })
                .into_iter()
                .collect(),
            rank_positions: [
                ("rank.dryad", 94),
                ("rank.sprite", 90),
                ("rank.sapling", 88),
            ]
            .map(|(key, position)| (key.to_string(), position))
            .into(),
            generated_at: Some(now - chrono::Duration::seconds(10)),
            synced_at: Some(now - chrono::Duration::minutes(10)),
            fetched_at: now,
        }
    }

    #[test]
    fn sprite_and_above_are_granted_and_below_or_unknown_is_denied() {
        let now = Utc::now();
        let decide = |held| rank_decision(&member(), Some(&feed(held, now)), now);
        assert_eq!(decide(Some(90)), WarAccess::Granted, "Sprite itself");
        assert_eq!(decide(Some(94)), WarAccess::Granted, "above Sprite");
        assert_eq!(decide(Some(88)), WarAccess::Denied, "Sapling");
        assert_eq!(decide(None), WarAccess::Denied, "no known rank");
    }

    #[test]
    fn the_uuid_matches_whatever_case_the_session_carries() {
        let now = Utc::now();
        let mut viewer = member();
        viewer.minecraft_uuid = MEMBER.to_ascii_uppercase();
        assert_eq!(
            rank_decision(&viewer, Some(&feed(Some(90), now)), now),
            WarAccess::Granted
        );
    }

    #[test]
    fn stale_or_unreadable_rank_evidence_is_an_outage_never_a_grant() {
        let now = Utc::now();
        assert_eq!(
            rank_decision(&member(), None, now),
            WarAccess::Unavailable,
            "never read"
        );
        let ttl = crate::config::RANK_ROLES_CACHE_TTL_SECS;
        let last_good = feed(Some(94), now - chrono::Duration::seconds(ttl));
        assert_eq!(
            rank_decision(&member(), Some(&last_good), now),
            WarAccess::Unavailable,
            "the roster's last good read, served past its freshness while the feed fails"
        );
        let mut stale = feed(Some(94), now);
        stale.stale = true;
        assert_eq!(
            rank_decision(&member(), Some(&stale), now),
            WarAccess::Unavailable,
            "a fresh read the feed flags stale"
        );
        let mut unplaced = feed(Some(94), now);
        unplaced.rank_positions.remove(WAR_RANK_FLOOR);
        assert_eq!(
            rank_decision(&member(), Some(&unplaced), now),
            WarAccess::Unavailable,
            "a catalog without the floor's position"
        );
        assert_eq!(
            rank_decision(&member(), Some(&feed(Some(94), now)), now),
            WarAccess::Granted,
            "the same rank, freshly read"
        );
    }

    #[test]
    fn a_frozen_feed_is_an_outage_however_recently_it_was_received() {
        // The backend keeps serving its last snapshot, stamps and `stale: false` included, when
        // it cannot build another; this server's own receipt time says nothing about that.
        let now = Utc::now();
        let decide = |change: fn(&mut CachedRankRoles, DateTime<Utc>)| {
            let mut read = feed(Some(94), now);
            change(&mut read, now);
            rank_decision(&member(), Some(&read), now)
        };
        assert_eq!(
            decide(|_, _| {}),
            WarAccess::Granted,
            "the healthy read itself"
        );
        assert_eq!(
            decide(|read, now| {
                read.generated_at =
                    Some(now - chrono::Duration::seconds(RANK_SNAPSHOT_MAX_AGE_SECS + 1));
            }),
            WarAccess::Unavailable,
            "a snapshot built past its bound"
        );
        assert_eq!(
            decide(|read, _| read.generated_at = None),
            WarAccess::Unavailable,
            "no snapshot stamp"
        );
        assert_eq!(
            decide(|read, now| {
                read.synced_at = Some(now - chrono::Duration::seconds(RANK_SYNC_MAX_AGE_SECS + 1));
            }),
            WarAccess::Unavailable,
            "Discord last synced past its bound"
        );
        assert_eq!(
            decide(|read, _| read.synced_at = None),
            WarAccess::Unavailable,
            "a guild with no recorded sync"
        );
        assert_eq!(
            decide(|read, now| {
                read.generated_at = Some(now + chrono::Duration::seconds(CLOCK_SKEW_SECS + 5));
            }),
            WarAccess::Unavailable,
            "a stamp from the future"
        );
        assert_eq!(
            decide(|read, now| read.generated_at = Some(now + chrono::Duration::seconds(5))),
            WarAccess::Granted,
            "ordinary clock skew"
        );
    }

    #[test]
    fn a_rank_counts_only_for_the_discord_account_the_feed_links_it_to() {
        let now = Utc::now();
        let mut relinked = member();
        relinked.discord_id = "2".to_string();
        assert_eq!(
            rank_decision(&relinked, Some(&feed(Some(94), now)), now),
            WarAccess::Unavailable,
            "another Discord account now holds this Minecraft UUID"
        );
        let mut unlinked = feed(Some(94), now);
        for ranked in unlinked.ranked.values_mut() {
            ranked.discord_id = None;
        }
        assert_eq!(
            rank_decision(&member(), Some(&unlinked), now),
            WarAccess::Unavailable,
            "a rank the feed links to no Discord account"
        );
    }

    #[test]
    fn refusals_keep_a_verdict_apart_from_an_outage() {
        assert_eq!(WarAccess::Granted.into_result(), Ok(()));
        assert_eq!(WarAccess::Denied.into_result(), Err(StatusCode::FORBIDDEN));
        assert_eq!(
            WarAccess::Unavailable.into_result(),
            Err(StatusCode::SERVICE_UNAVAILABLE)
        );
        assert_eq!(WarAccess::Unavailable.known(), None);
    }

    const FEED_DOWN: u8 = 0;
    const FEED_STALE: u8 = 1;
    const FEED_UP: u8 = 2;
    /// Answers 200 with `stale: false` from a snapshot built ten minutes ago, as the backend
    /// does when it cannot rebuild.
    const FEED_FROZEN: u8 = 3;
    /// Answers as [`FEED_UP`], except that `sprite` has been lowered to Sapling.
    const FEED_DEMOTED: u8 = 4;
    const UUIDS: [(&str, &str); 9] = [
        ("outsider", "00000000-0000-0000-0000-000000000001"),
        ("sapling", "00000000-0000-0000-0000-000000000002"),
        ("sprite", "00000000-0000-0000-0000-000000000003"),
        ("dryad", "00000000-0000-0000-0000-000000000004"),
        ("unranked", "00000000-0000-0000-0000-000000000005"),
        ("admin", "00000000-0000-0000-0000-000000000006"),
        ("chief-sapling", "00000000-0000-0000-0000-000000000007"),
        // A Discord account newly linked to `sprite`'s Minecraft account.
        ("relinked", "00000000-0000-0000-0000-000000000003"),
        ("nodiscord", "00000000-0000-0000-0000-000000000008"),
    ];

    /// A stand-in backend whose `/auth/web/me` answers by bearer token, each with its own Discord
    /// account, and a rank feed in the published shape with the live catalog's positions around
    /// the floor, behaving as `feed` says.
    async fn spawn_upstreams(feed: Arc<AtomicU8>) -> std::net::SocketAddr {
        async fn me(headers: HeaderMap) -> Response {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .unwrap_or_default();
            let (guild_rank, website_admin) = match bearer {
                "outsider" => (None, false),
                "sapling" | "sprite" | "unranked" | "relinked" | "nodiscord" => {
                    (Some("recruit"), false)
                }
                "dryad" => (Some("captain"), false),
                "chief-sapling" => (Some("chief"), false),
                "admin" => (None, true),
                "down" => return StatusCode::BAD_GATEWAY.into_response(),
                _ => return StatusCode::UNAUTHORIZED.into_response(),
            };
            let uuid = UUIDS
                .iter()
                .find(|(name, _)| *name == bearer)
                .map(|(_, uuid)| *uuid);
            axum::Json(serde_json::json!({
                "discord_id": format!("d-{bearer}"),
                "minecraft_uuid": uuid,
                "guild_rank": guild_rank,
                "website_admin": website_admin,
            }))
            .into_response()
        }
        let profile = |name: &str, rank: &str| {
            let uuid = UUIDS
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, uuid)| *uuid);
            let discord =
                (name != "nodiscord").then(|| serde_json::json!({"id": format!("d-{name}")}));
            serde_json::json!({"discord": discord, "minecraft": {"uuid": uuid},
                               "role_keys": [rank], "summary": {"progression_rank": rank}})
        };
        let body = serde_json::json!({
            "source": {"stale": false, "guilds": [
                {"guild_id": "1030187086722109570"}, {"guild_id": "1104432054717927544"}
            ]},
            "catalog": {"roles": [
                {"key": "rank.dryad", "label": "Dryad", "category": "progression_rank", "position": 94},
                {"key": "rank.sprite", "label": "Sprite", "category": "progression_rank", "position": 90},
                {"key": "rank.sapling", "label": "Sapling", "category": "progression_rank", "position": 88}
            ]},
            "profiles": [
                profile("outsider", "rank.dryad"),
                profile("sapling", "rank.sapling"),
                profile("sprite", "rank.sprite"),
                profile("dryad", "rank.dryad"),
                profile("chief-sapling", "rank.sapling"),
                profile("nodiscord", "rank.sprite")
            ]
        });
        let upstreams = Router::new().route("/auth/web/me", get(me)).route(
            "/v1/rank-profiles",
            get(move || {
                let (mut body, feed) = (body.clone(), Arc::clone(&feed));
                async move {
                    let mode = feed.load(Ordering::SeqCst);
                    if mode == FEED_DOWN {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let now = Utc::now();
                    let built = if mode == FEED_FROZEN {
                        now - chrono::Duration::minutes(10)
                    } else {
                        now
                    };
                    body["generated_at"] = built.to_rfc3339().into();
                    for guild in body["source"]["guilds"]
                        .as_array_mut()
                        .into_iter()
                        .flatten()
                    {
                        guild["last_full_sync_at"] =
                            (built - chrono::Duration::minutes(5)).to_rfc3339().into();
                    }
                    body["source"]["stale"] = (mode == FEED_STALE).into();
                    if mode == FEED_DEMOTED {
                        body["profiles"][2]["summary"]["progression_rank"] = "rank.sapling".into();
                    }
                    axum::Json(body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move { axum::serve(listener, upstreams).await.expect("serve") });
        addr
    }

    /// Runs `body` against the real app with the stand-in upstreams, Sequoia's roster cached and
    /// a war feed state cached, so a granted request reaches each handler.
    fn with_app<F>(feed: u8, body: impl FnOnce(String, AppState, Arc<AtomicU8>) -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        with_app_env(feed, &[], body);
    }

    /// [`with_app`], with more environment for the app.
    fn with_app_env<F>(
        feed: u8,
        env: &[(&str, &str)],
        body: impl FnOnce(String, AppState, Arc<AtomicU8>) -> F,
    ) where
        F: std::future::Future<Output = ()>,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let feed = Arc::new(AtomicU8::new(feed));
        let upstreams = runtime.block_on(spawn_upstreams(Arc::clone(&feed)));
        let upstream_url = format!("http://{upstreams}");
        temp_env::with_vars(
            env.iter()
                .map(|(name, value)| (*name, Some(*value)))
                .chain([
                    ("SEQUOIA_BACKEND_BASE_URL", Some(upstream_url.as_str())),
                    (
                        "SEQUOIA_RANK_PROFILES_BASE_URL",
                        Some(upstream_url.as_str()),
                    ),
                ])
                .collect::<Vec<_>>(),
            || {
                runtime.block_on(async {
                    let state = AppState::new(None);
                    state.guild_cache.insert(
                        crate::config::SEQUOIA_GUILD_NAME.to_string(),
                        crate::state::CachedGuild {
                            data: format!(r#"{{"members":{{"chief":{{"EpicPuppy613":{{"uuid":"{MEMBER}"}}}}}}}}"#),
                            fetched_at: Utc::now(),
                            observed_at: Utc::now(),
                        },
                    );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("bind");
                    let addr = listener.local_addr().expect("address");
                    let app = crate::app::build_app(state.clone());
                    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
                    body(format!("http://{addr}"), state, feed).await;
                });
            },
        );
    }

    async fn ask(base: &str, session: Option<&str>, path: &str) -> reqwest::Response {
        let mut request = reqwest::Client::new().get(format!("{base}{path}"));
        if let Some(session) = session {
            request = request.header("cookie", format!("seq_session={session}"));
        }
        request.send().await.expect("request")
    }

    /// Every war route through the real app, for each kind of viewer. A granted request reaches
    /// its handler: the war feed's cached state, the roster's roles, or a `400` for a malformed
    /// query that only the handler itself checks.
    #[test]
    fn every_war_route_answers_only_sprite_and_above_and_admins_privately() {
        with_app(FEED_UP, |base, _state, _feed| async move {
            let routes = [
                ("/api/warcontroller", 200),
                ("/api/war-activity?since=-1", 400),
                ("/api/members", 200),
                ("/api/members/bad%20name", 400),
                ("/api/members/EpicPuppy613/war?period=forever", 400),
            ];
            for (session, granted) in [
                (None, false),
                (Some("expired"), false),
                (Some("outsider"), false),
                (Some("sapling"), false),
                (Some("chief-sapling"), false),
                (Some("unranked"), false),
                (Some("sprite"), true),
                (Some("dryad"), true),
                (Some("admin"), true),
            ] {
                for (path, reached) in routes {
                    let response = ask(&base, session, path).await;
                    let expected = if granted { reached } else { 403 };
                    assert_eq!(response.status().as_u16(), expected, "{session:?} {path}");
                    let headers = response.headers();
                    assert_eq!(
                        headers["cache-control"], "private, no-store",
                        "{session:?} {path}"
                    );
                    assert_eq!(headers["vary"], "Cookie", "{session:?} {path}");
                }
            }
            for session in ["down", "relinked", "nodiscord"] {
                for (path, _) in routes {
                    let status = ask(&base, Some(session), path).await.status().as_u16();
                    assert_eq!(status, 503, "{session}: {path}");
                }
            }
        });
    }

    #[test]
    fn a_frozen_feed_answering_200_is_still_an_outage() {
        with_app(FEED_FROZEN, |base, state, _feed| async move {
            for session in ["sprite", "dryad", "sapling"] {
                let status = ask(&base, Some(session), "/api/warcontroller")
                    .await
                    .status();
                assert_eq!(
                    status.as_u16(),
                    503,
                    "{session}, snapshot built ten minutes ago"
                );
            }
            let read = state
                .rank_roles_cache
                .read()
                .await
                .clone()
                .expect("the read is held");
            assert!(!read.stale && super::super::members::is_fresh(&read, Utc::now()));
            assert_eq!(
                ask(&base, Some("admin"), "/api/warcontroller")
                    .await
                    .status(),
                200
            );
            assert_eq!(
                ask(&base, Some("outsider"), "/api/warcontroller")
                    .await
                    .status(),
                403
            );
        });
    }

    #[test]
    fn a_stale_or_failing_feed_answers_members_503_and_admins_and_outsiders_as_before() {
        with_app(FEED_STALE, |base, state, feed| async move {
            for session in ["sprite", "dryad", "sapling"] {
                let status = ask(&base, Some(session), "/api/warcontroller")
                    .await
                    .status();
                assert_eq!(status.as_u16(), 503, "{session}, feed flags itself stale");
            }
            assert_eq!(
                ask(&base, Some("admin"), "/api/warcontroller")
                    .await
                    .status(),
                200
            );
            assert_eq!(
                ask(&base, Some("outsider"), "/api/warcontroller")
                    .await
                    .status(),
                403
            );
            assert_eq!(state.war_access.len(), 2, "no member outage is remembered");

            // The roster keeps serving its last good read while the feed fails; the gate must
            // not once that read is past its minute.
            feed.store(FEED_UP, Ordering::SeqCst);
            *state.rank_roles_cache.write().await = None;
            state.war_access.clear();
            assert_eq!(
                ask(&base, Some("sprite"), "/api/warcontroller")
                    .await
                    .status(),
                200
            );
            feed.store(FEED_DOWN, Ordering::SeqCst);
            state.war_access.clear();
            if let Some(read) = state.rank_roles_cache.write().await.as_mut() {
                read.fetched_at -=
                    chrono::Duration::seconds(crate::config::RANK_ROLES_CACHE_TTL_SECS);
            }
            let status = ask(&base, Some("sprite"), "/api/warcontroller")
                .await
                .status();
            assert_eq!(status.as_u16(), 503, "a last good read past its minute");
        });
    }

    #[test]
    fn answers_are_remembered_per_session_by_digest_except_an_outage() {
        with_app(FEED_UP, |_base, state, _feed| async move {
            let session = |token: &str| {
                let mut headers = HeaderMap::new();
                let cookie = format!("seq_session={token}");
                headers.insert(
                    axum::http::header::COOKIE,
                    cookie.parse().expect("cookie header"),
                );
                headers
            };
            assert_eq!(
                war_access(&state, &session("sprite")).await,
                WarAccess::Granted
            );
            assert_eq!(
                war_access(&state, &session("sapling")).await,
                WarAccess::Denied
            );
            assert_eq!(
                war_access(&state, &session("down")).await,
                WarAccess::Unavailable
            );
            assert_eq!(
                war_access(&state, &HeaderMap::new()).await,
                WarAccess::Denied
            );
            assert_eq!(
                state.war_access.len(),
                2,
                "the outage and the signed-out are not kept"
            );
            let digest: [u8; 32] = Sha256::digest(b"sprite").into();
            assert!(
                state.war_access.contains_key(&digest),
                "kept by digest, never by token"
            );
        });
    }

    /// The SSE stream's first war frame: sent to a granted viewer at connect, never to anyone
    /// else, while every viewer still gets the public territory snapshot.
    #[test]
    fn the_stream_seeds_war_state_only_for_granted_viewers() {
        with_app(FEED_UP, |base, state, _feed| async move {
            state.live_snapshot.write().await.snapshot_json =
                Arc::new(bytes::Bytes::from_static(br#"{"territories":{}}"#));
            let war = sequoia_shared::WarControllerState::unavailable(1);
            *state.warcontroller_cache.write().await = Some(crate::state::CachedWarController {
                json: Arc::new(bytes::Bytes::from(serde_json::to_vec(&war).expect("json"))),
                state: war,
                fetched_at: Utc::now(),
            });
            for (session, granted) in [
                (None, false),
                (Some("outsider"), false),
                (Some("chief-sapling"), false),
                (Some("unranked"), false),
                (Some("sprite"), true),
                (Some("admin"), true),
            ] {
                let mut response = ask(&base, session, "/api/events").await;
                let mut seen = String::new();
                let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
                while let Ok(Ok(Some(chunk))) =
                    tokio::time::timeout_at(deadline, response.chunk()).await
                {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                }
                assert!(seen.contains("event: snapshot"), "{session:?}: {seen}");
                assert_eq!(
                    seen.contains("event: warcontroller"),
                    granted,
                    "{session:?}: {seen}"
                );
            }
        });
    }

    /// Whatever a body yields within `wait` milliseconds.
    async fn read_for(body: &mut axum::body::BodyDataStream, wait: u64) -> String {
        use futures::StreamExt;

        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
        while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        seen
    }

    /// A demotion that lands while a stream is lagging: the replay that follows the lag must go
    /// through the same overdue re-check as a live frame, so the stream sends its clearing frame
    /// and never the cached war state.
    #[test]
    fn an_overdue_recheck_runs_before_a_lag_replay_on_a_real_stream() {
        let env = [
            ("WAR_FEED_REVALIDATE_SECS", "1"),
            ("SSE_BROADCAST_BUFFER", "4"),
        ];
        with_app_env(FEED_UP, &env, |_base, state, feed| async move {
            let war: sequoia_shared::WarControllerState = serde_json::from_str(
                r#"{"timestamp":100,"players":[{"username":"PrivateIntel","class":"WARRIOR"}]}"#,
            )
            .expect("war state");
            *state.warcontroller_cache.write().await = Some(crate::state::CachedWarController {
                json: Arc::new(bytes::Bytes::from(serde_json::to_vec(&war).expect("json"))),
                state: war,
                fetched_at: Utc::now(),
            });
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::COOKIE,
                "seq_session=sprite".parse().expect("cookie"),
            );
            let response =
                crate::routes::sse::territory_events(axum::extract::State(state.clone()), headers)
                    .await;
            let mut body = response.into_body().into_data_stream();
            assert!(
                read_for(&mut body, 500).await.contains("PrivateIntel"),
                "seeded while granted"
            );

            // Demoted, with both remembered answers lapsed and the re-check overdue.
            feed.store(FEED_DEMOTED, Ordering::SeqCst);
            *state.rank_roles_cache.write().await = None;
            state.war_access.clear();
            tokio::time::sleep(Duration::from_millis(1100)).await;
            // More broadcasts than the buffer holds, none read yet: the stream lags.
            for seq in 0..8 {
                let json = Arc::new(bytes::Bytes::from_static(b"{}"));
                let _ = state
                    .event_tx
                    .send(crate::state::PreSerializedEvent::Update { seq, json });
            }
            let after = read_for(&mut body, 1000).await;
            assert!(
                after.contains(r#""unavailable":true"#),
                "the clearing frame goes out: {after}"
            );
            assert!(
                !after.contains("PrivateIntel"),
                "the cached war state is not replayed: {after}"
            );
        });
    }
}
