//! Website sign-in, as seen from the map client.
//!
//! There is no login form here. `sequoia-backend` owns the account (Discord
//! OAuth linked to a Minecraft UUID) and issues the `seq_session` cookie for
//! `.seqwawa.com`; the map server exchanges that HttpOnly cookie for an
//! identity on `/api/auth/me`. This module is display-only - nothing on the
//! map is gated on the viewer.

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
}

#[derive(Deserialize)]
struct MeResponse {
    #[serde(default)]
    viewer: Option<Viewer>,
}

/// Resolves the current website session, or `None` when signed out.
///
/// Any failure - offline, backend down, unparseable body - reads as signed out.
/// The map is fully usable either way, so there is nothing to retry.
pub async fn fetch_viewer() -> Option<Viewer> {
    let response = gloo_net::http::Request::get("/api/auth/me")
        .send()
        .await
        .ok()?;
    if !response.ok() {
        return None;
    }
    response.json::<MeResponse>().await.ok()?.viewer
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
    fn login_url_encodes_the_return_destination() {
        assert_eq!(
            login_url("https://map.seqwawa.com/history?t=5"),
            "/api/auth/login?return_to=https%3A%2F%2Fmap.seqwawa.com%2Fhistory%3Ft%3D5"
        );
    }

    #[test]
    fn nmsr_face_builds_a_sized_render_url() {
        assert_eq!(
            nmsr_face("ee860b7c-9a1d-49cf-9f19-ab673ba0f23b", 18),
            "https://nmsr.seqwawa.com/face/ee860b7c-9a1d-49cf-9f19-ab673ba0f23b?w=18"
        );
    }
}
