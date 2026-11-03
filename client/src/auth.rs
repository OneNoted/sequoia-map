//! Website sign-in, as seen from the map client.
//!
//! There is no login form here. `sequoia-backend` owns the account (Discord
//! OAuth linked to a Minecraft UUID) and issues the `seq_session` cookie for
//! `.seqwawa.com`; the map server exchanges that HttpOnly cookie for an
//! identity on `/api/auth/me`, with whether it may see war features. The server
//! is the real gate on those; what the session says here is what the map bothers
//! to render - the account chip, and the war features' presence.

use serde::Deserialize;

use crate::encode_uri_component;

/// Self-hosted skin renderer (github.com/NickAcPT/nmsr-rs), same host the
/// website's avatars come from.
pub(crate) const NMSR_BASE_URL: &str = "https://nmsr.seqwawa.com";

/// Signed-in identity carried by the website session.
///
/// Field names mirror the backend wire shape so this stays comparable with the
/// website's own `Viewer`. `website_admin` gates the navbar's Manage link, the
/// same way the website's `manage` prop does.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

/// The current website session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Session {
    /// `None` when signed out.
    #[serde(default)]
    pub viewer: Option<Viewer>,
    /// Whether the server lets this session see war features: Sequoia members ranked Sprite or
    /// above, and website administrators. `None` while the server could not tell.
    ///
    /// Display-side only. The server refuses war data on its own; this keeps the map from
    /// asking for, or reserving space for, data it cannot have.
    #[serde(default)]
    pub war_access: Option<bool>,
}

/// Resolves the current website session, `Err` when the answer could not be obtained.
///
/// The two are kept apart because they call for opposite handling. A signed-out answer is
/// final. A failure - offline, server restarting, unreadable body - is not, and collapsing it
/// into "signed out" would hide the war feed from a member until they reloaded the page, so
/// the caller retries instead.
pub async fn fetch_session() -> Result<Session, String> {
    let response = gloo_net::http::Request::get("/api/auth/me")
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    // 4xx is an authoritative "not signed in"; 5xx is the server having a bad moment.
    if response.status() >= 500 {
        return Err(format!("status {}", response.status()));
    }
    if !response.ok() {
        return Ok(Session {
            viewer: None,
            war_access: Some(false),
        });
    }
    response
        .json::<Session>()
        .await
        .map_err(|error| format!("decode failed: {error}"))
}

/// Starts sign-in and returns the browser to `return_to` (an absolute map URL).
pub fn login_url(return_to: &str) -> String {
    format!(
        "/api/auth/login?return_to={}",
        encode_uri_component(return_to)
    )
}

/// Clears the session and returns to the map root.
///
/// Returning to the current URL would be wrong for a page that only exists for
/// the signed-in viewer; the root is always public.
pub fn logout_url() -> String {
    "/api/auth/logout?return_to=%2F".to_string()
}

/// The current page URL, used as the sign-in `return_to` so the viewer lands
/// back where they were - same territory selection, same history timestamp.
#[cfg(target_arch = "wasm32")]
pub fn current_url() -> String {
    web_sys::window()
        .and_then(|window| window.location().href().ok())
        .unwrap_or_else(|| "/".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn current_url() -> String {
    "/".to_string()
}

/// Flat face render for the account chip.
pub fn nmsr_face(identifier: &str, size: u32) -> String {
    format!(
        "{NMSR_BASE_URL}/face/{}?w={size}",
        encode_uri_component(identifier)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_reads_war_access_as_the_server_states_it() {
        let session: Session = serde_json::from_str(
            r#"{"viewer":{"discord_id":"1","minecraft_uuid":"ee860b7c-9a1d-49cf-9f19-ab673ba0f23b","guild_rank":"chief"},"war_access":false}"#,
        )
        .expect("session parses");
        assert_eq!(
            session.war_access,
            Some(false),
            "a high in-game rank grants nothing"
        );
        let unknown: Session =
            serde_json::from_str(r#"{"viewer":null,"war_access":null}"#).expect("unknown parses");
        assert_eq!(unknown.war_access, None);
    }

    #[test]
    fn a_viewer_deserializes_without_a_guild_rank() {
        let viewer: Viewer = serde_json::from_str(
            r#"{"discord_id":"1","minecraft_uuid":"ee860b7c-9a1d-49cf-9f19-ab673ba0f23b"}"#,
        )
        .expect("legacy viewer payload parses");
        assert_eq!(viewer.guild_rank, None);
    }

    #[test]
    fn login_url_encodes_the_return_destination() {
        assert_eq!(
            login_url("https://map.seqwawa.com/history?t=5"),
            "/api/auth/login?return_to=https%3A%2F%2Fmap.seqwawa.com%2Fhistory%3Ft%3D5"
        );
    }
}
