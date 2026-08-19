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
    let viewer = match session_cookie(&headers) {
        Some(token) => fetch_viewer(&state, token).await,
        None => None,
    };
    session_json(serde_json::json!({ "viewer": viewer }))
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
    let Some(base_url) = config::sequoia_backend_base_url() else {
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
