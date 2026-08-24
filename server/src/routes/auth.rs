//! Website sign-in, proxied to the Sequoia backend.
//!
//! The map has no accounts of its own. `sequoia-backend` issues the `seq_session`
//! JWT cookie for `.seqwawa.com`, so the browser already sends it to the map
//! origin - but it is HttpOnly, so the wasm client cannot read it. These routes
//! let the server do the reading: `/api/auth/me` exchanges the cookie for the
//! viewer identity, and login/logout are thin redirects that keep the backend
//! base URL out of the client bundle.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config;
use crate::state::AppState;

const SESSION_COOKIE: &str = "seq_session";
/// Matches the website's own session probe timeout (`viewerSession.ts`); a slow
/// backend must degrade to "signed out", never stall the map's first paint.
const ME_TIMEOUT: Duration = Duration::from_secs(3);

/// Website identity, in the backend's wire shape so both clients stay comparable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewer {
    pub discord_id: String,
    #[serde(default)]
    pub discord_username: Option<String>,
    pub minecraft_uuid: String,
    #[serde(default)]
    pub minecraft_username: Option<String>,
    #[serde(default)]
    pub website_admin: bool,
    /// In-game guild rank, present only while the viewer is on the Sequoia roster.
    #[serde(default)]
    pub guild_rank: Option<String>,
}

impl Viewer {
    /// Sequoia membership: any in-game guild rank, or a website administrator - the same
    /// application-wide staff override the website's own branch gate applies.
    ///
    /// A backend that does not send `guild_rank` therefore fails closed for everyone but
    /// admins, which is the right way round for internal war data.
    pub fn is_guild_member(&self) -> bool {
        self.website_admin || self.guild_rank.is_some()
    }
}

/// Whether the request carries a session belonging to a Sequoia member. Signed-out
/// viewers, and signed-in outsiders, are both denied.
pub fn viewer_is_guild_member(viewer: Option<&Viewer>) -> bool {
    viewer.is_some_and(Viewer::is_guild_member)
}

#[derive(Debug, Deserialize)]
pub struct ReturnToQuery {
    return_to: Option<String>,
}

/// Resolves the `seq_session` cookie to a verified identity, or null.
///
/// Never fails: an unset backend URL, a timeout, a non-2xx, or an unparseable
/// body all render the viewer as signed out. The map is fully usable that way.
pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let viewer = resolve_viewer(&state, &headers).await;
    session_json(serde_json::json!({ "viewer": viewer }))
}

/// Resolves the request's `seq_session` cookie to a verified identity, or `None`.
///
/// The single entry point for "who is asking" - route handlers must not re-read the
/// cookie themselves. Costs one backend round trip (capped at [`ME_TIMEOUT`]), so call it
/// off the critical path of anything that has to render immediately.
pub async fn resolve_viewer(state: &AppState, headers: &HeaderMap) -> Option<Viewer> {
    let token = session_cookie(headers)?;
    fetch_viewer(state, token).await
}

/// Starts the website sign-in flow and returns the browser to a map page.
pub async fn login(Query(query): Query<ReturnToQuery>) -> Response {
    redirect_to_auth("start", query.return_to.as_deref())
}

/// Clears the website session and returns the browser to a map page.
pub async fn logout(Query(query): Query<ReturnToQuery>) -> Response {
    redirect_to_auth("logout", query.return_to.as_deref())
}

async fn fetch_viewer(state: &AppState, token: &str) -> Option<Viewer> {
    let base_url = config::sequoia_backend_base_url()?;
    let response = state
        .http_client
        .get(format!("{base_url}/auth/web/me"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .timeout(ME_TIMEOUT)
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!("website session probe failed: {error}");
            return None;
        }
    };
    if !response.status().is_success() {
        return None;
    }

    match response.json::<Viewer>().await {
        // The backend can only answer for a session it linked to a player, but
        // a blank id would still deserialize - treat it as no session.
        Ok(viewer) if !viewer.discord_id.is_empty() && !viewer.minecraft_uuid.is_empty() => {
            Some(viewer)
        }
        Ok(_) => None,
        Err(error) => {
            warn!("website session probe returned an unreadable body: {error}");
            None
        }
    }
}

fn redirect_to_auth(action: &str, return_to: Option<&str>) -> Response {
    // The browser follows this, so it needs the publicly reachable origin, which is not
    // necessarily the one this server polls over an internal network.
    let Some(base_url) = config::sequoia_backend_public_base_url() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "website sign-in is not configured",
        )
            .into_response();
    };
    let destination = safe_return_to(return_to);
    let url = format!(
        "{base_url}/auth/web/{action}?return_to={}",
        urlencoding_encode(&destination)
    );
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, url),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response()
}

/// A response that carries the viewer must never be cached or shared: a proxy
/// that keyed it by path alone would hand one visitor another's identity.
fn session_json(body: serde_json::Value) -> Response {
    (
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::VARY, "Cookie"),
        ],
        axum::Json(body),
    )
        .into_response()
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == SESSION_COOKIE)
            .then_some(value.trim())
            .filter(|value| !value.is_empty())
    })
}

/// The backend only accepts `return_to` on its allowlisted hosts, so every
/// destination is resolved against this map's own public origin here - a
/// foreign or malformed value can never leak into the redirect.
fn safe_return_to(raw: Option<&str>) -> String {
    let base = config::map_public_base_url();
    let root = format!("{base}/");
    let Some(candidate) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return root;
    };

    // Only absolute URLs can carry a foreign origin; a path is by definition ours.
    if let Some(rest) = candidate.strip_prefix(&format!("{base}/")) {
        // Reject a protocol-relative smuggle such as `https://map//evil.com`.
        if rest.starts_with('/') {
            return root;
        }
        return candidate.to_string();
    }
    if candidate == base || candidate == root {
        return root;
    }
    if candidate.starts_with("http://") || candidate.starts_with("https://") {
        return root;
    }
    if candidate.starts_with("//") || !candidate.starts_with('/') {
        return root;
    }
    format!("{base}{candidate}")
}

/// Percent-encodes a `return_to` for use in a query string. The set of
/// characters that matter here is small and fixed, so this avoids a dependency.
fn urlencoding_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() + 16);
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_cookie(raw: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(raw).unwrap());
        headers
    }

    fn viewer_with(guild_rank: Option<&str>, website_admin: bool) -> Viewer {
        Viewer {
            discord_id: "1".to_string(),
            discord_username: None,
            minecraft_uuid: "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b".to_string(),
            minecraft_username: None,
            website_admin,
            guild_rank: guild_rank.map(str::to_string),
        }
    }

    #[test]
    fn any_guild_rank_makes_a_viewer_a_member() {
        assert!(viewer_with(Some("chief"), false).is_guild_member());
        assert!(viewer_with(Some("recruit"), false).is_guild_member());
    }

    #[test]
    fn a_website_admin_is_a_member_without_a_rank() {
        assert!(viewer_with(None, true).is_guild_member());
    }

    #[test]
    fn a_signed_in_outsider_is_not_a_member() {
        // The backend answers for any linked account, not just Sequoia's roster, and a
        // deploy predating the `guild_rank` field sends none at all - both must fail closed.
        assert!(!viewer_with(None, false).is_guild_member());
        assert!(!viewer_is_guild_member(Some(&viewer_with(None, false))));
    }

    #[test]
    fn a_signed_out_visitor_is_not_a_member() {
        assert!(!viewer_is_guild_member(None));
    }

    #[test]
    fn a_viewer_deserializes_without_a_guild_rank() {
        let viewer: Viewer = serde_json::from_str(
            r#"{"discord_id":"1","minecraft_uuid":"ee860b7c-9a1d-49cf-9f19-ab673ba0f23b"}"#,
        )
        .expect("legacy viewer payload parses");
        assert_eq!(viewer.guild_rank, None);
        assert!(!viewer.is_guild_member());
    }

    #[test]
    fn session_cookie_reads_a_lone_cookie() {
        let headers = headers_with_cookie("seq_session=abc.def.ghi");
        assert_eq!(session_cookie(&headers), Some("abc.def.ghi"));
    }

    #[test]
    fn session_cookie_reads_past_neighbours_and_spaces() {
        let headers = headers_with_cookie("theme=dark; seq_session=token; other=1");
        assert_eq!(session_cookie(&headers), Some("token"));
    }

    #[test]
    fn session_cookie_ignores_a_prefix_match() {
        let headers = headers_with_cookie("seq_session_old=stale");
        assert_eq!(session_cookie(&headers), None);
    }

    #[test]
    fn session_cookie_treats_an_empty_value_as_absent() {
        let headers = headers_with_cookie("seq_session=");
        assert_eq!(session_cookie(&headers), None);
    }

    #[test]
    fn session_cookie_handles_a_missing_header() {
        assert_eq!(session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn safe_return_to_defaults_to_the_map_root() {
        temp_env::with_var("MAP_DOMAIN", Some("map.seqwawa.com"), || {
            assert_eq!(safe_return_to(None), "https://map.seqwawa.com/");
            assert_eq!(safe_return_to(Some("   ")), "https://map.seqwawa.com/");
        });
    }

    #[test]
    fn safe_return_to_keeps_our_own_absolute_urls() {
        temp_env::with_var("MAP_DOMAIN", Some("map.seqwawa.com"), || {
            assert_eq!(
                safe_return_to(Some("https://map.seqwawa.com/history?t=5")),
                "https://map.seqwawa.com/history?t=5"
            );
        });
    }

    #[test]
    fn safe_return_to_resolves_relative_paths_against_the_map_origin() {
        temp_env::with_var("MAP_DOMAIN", Some("map.seqwawa.com"), || {
            assert_eq!(
                safe_return_to(Some("/history")),
                "https://map.seqwawa.com/history"
            );
        });
    }

    #[test]
    fn safe_return_to_rejects_foreign_and_smuggled_origins() {
        temp_env::with_var("MAP_DOMAIN", Some("map.seqwawa.com"), || {
            let root = "https://map.seqwawa.com/";
            assert_eq!(safe_return_to(Some("https://evil.example/steal")), root);
            assert_eq!(safe_return_to(Some("//evil.example/steal")), root);
            assert_eq!(
                safe_return_to(Some("https://map.seqwawa.com//evil.example")),
                root
            );
            // A host that merely starts with ours must not pass.
            assert_eq!(
                safe_return_to(Some("https://map.seqwawa.com.evil.example/x")),
                root
            );
            assert_eq!(safe_return_to(Some("javascript:alert(1)")), root);
        });
    }

    #[test]
    fn urlencoding_encode_escapes_query_delimiters() {
        assert_eq!(
            urlencoding_encode("https://map.seqwawa.com/history?t=5&x=1"),
            "https%3A%2F%2Fmap.seqwawa.com%2Fhistory%3Ft%3D5%26x%3D1"
        );
    }
}
