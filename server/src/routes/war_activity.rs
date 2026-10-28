//! Recent war activity for members: one bounded page at a time from the backend's journal.
//!
//! The same member gate as the war feed. The query is checked and bounded here, forwarded with
//! the internal token, and the answer is parsed into the shared types before it goes out, so
//! the browser only ever receives the agreed shape. Nothing is cached or stored on this side.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use sequoia_shared::war_activity::WarActivityPage;
use serde::Deserialize;
use tracing::warn;

use super::http_util::private_json_bytes_response;
use crate::config;
use crate::state::AppState;

const UPSTREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_LIMIT: u32 = 50;
const MAX_TERRITORY_LEN: usize = 64;
const MAX_CURSOR_LEN: usize = 40;

#[derive(Debug, Default, Deserialize)]
pub struct ActivityQuery {
    since: Option<i64>,
    before: Option<String>,
    limit: Option<u32>,
    territory: Option<String>,
    scope: Option<String>,
}

pub async fn get_war_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ActivityQuery>,
) -> Response {
    let probe = super::auth::resolve_viewer(&state, &headers).await;
    if probe.is_unavailable() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !super::auth::viewer_is_guild_member(probe.viewer()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(params) = upstream_params(query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(url) = config::war_activity_url() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let mut request = state
        .http_client
        .get(url)
        .query(&params)
        .timeout(UPSTREAM_TIMEOUT);
    if let Some(token) = config::sequoia_backend_internal_token() {
        request = request.bearer_auth(token);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            warn!("war activity request failed: {error}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = response.status();
    if status == StatusCode::BAD_REQUEST {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !status.is_success() {
        warn!("war activity upstream answered {status}");
        return StatusCode::BAD_GATEWAY.into_response();
    }
    match response.bytes().await.map(|bytes| checked(&bytes)) {
        Ok(Some(body)) => private_json_bytes_response(body),
        Ok(None) => {
            warn!("war activity upstream sent an unreadable page");
            StatusCode::BAD_GATEWAY.into_response()
        }
        Err(error) => {
            warn!("war activity body failed: {error}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// The page re-encoded from the shared types, or `None` when it does not parse as one.
fn checked(body: &[u8]) -> Option<Bytes> {
    let page: WarActivityPage = serde_json::from_slice(body).ok()?;
    serde_json::to_vec(&page).ok().map(Bytes::from)
}

/// The backend query for a browser's, with every value checked and bounded.
fn upstream_params(query: ActivityQuery) -> Result<Vec<(&'static str, String)>, ()> {
    let mut params = Vec::new();
    if let Some(since) = query.since {
        if since < 0 {
            return Err(());
        }
        params.push(("since", since.to_string()));
    }
    if let Some(before) = query.before {
        let valid = !before.is_empty()
            && before.len() <= MAX_CURSOR_LEN
            && before.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
        if !valid {
            return Err(());
        }
        params.push(("before", before));
    }
    if let Some(limit) = query.limit {
        params.push(("limit", limit.clamp(1, MAX_LIMIT).to_string()));
    }
    if let Some(territory) = query.territory.map(|value| value.trim().to_string()) {
        if territory.is_empty() || territory.len() > MAX_TERRITORY_LEN {
            return Err(());
        }
        params.push(("territory", territory));
    }
    match query.scope.as_deref() {
        None | Some("decisive") => {}
        Some("all") => params.push(("scope", "all".to_string())),
        Some(_) => return Err(()),
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> ActivityQuery {
        let value = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        };
        ActivityQuery {
            since: value("since").map(|since| since.parse().expect("number")),
            before: value("before"),
            limit: value("limit").map(|limit| limit.parse().expect("number")),
            territory: value("territory"),
            scope: value("scope"),
        }
    }

    #[test]
    fn a_full_query_is_forwarded_with_its_limit_bounded() {
        let params = upstream_params(query(&[
            ("since", "1787500000"),
            ("before", "1787519000000.11"),
            ("limit", "500"),
            ("territory", "Detlas"),
            ("scope", "all"),
        ]))
        .expect("valid");
        assert_eq!(
            params,
            vec![
                ("since", "1787500000".to_string()),
                ("before", "1787519000000.11".to_string()),
                ("limit", "50".to_string()),
                ("territory", "Detlas".to_string()),
                ("scope", "all".to_string()),
            ]
        );
        assert_eq!(upstream_params(ActivityQuery::default()), Ok(Vec::new()));
    }

    #[test]
    fn malformed_values_are_refused_rather_than_forwarded() {
        for pairs in [
            &[("before", "1;DROP")][..],
            &[("scope", "everything")][..],
            &[("territory", " ")][..],
            &[("since", "-5")][..],
        ] {
            assert_eq!(upstream_params(query(pairs)), Err(()), "{pairs:?}");
        }
        let long = "x".repeat(MAX_TERRITORY_LEN + 1);
        assert_eq!(
            upstream_params(query(&[("territory", long.as_str())])),
            Err(())
        );
    }

    #[test]
    fn only_a_page_in_the_agreed_shape_reaches_the_browser() {
        let page = br#"{"server_time":10,"entries":[],"next_before":null,"kept_since":1,"extra":"dropped"}"#;
        let body = checked(page).expect("a page");
        let echoed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(echoed.get("extra").is_none());
        assert_eq!(checked(br#"{"error":"nope"}"#), None);
    }
}
