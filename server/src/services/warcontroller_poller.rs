use std::sync::Arc;

use bytes::Bytes;
use sequoia_shared::WarControllerState;
use tracing::{debug, info, warn};

use crate::config::{
    sequoia_backend_internal_token, warcontroller_poll_interval, warcontroller_url,
};
use crate::state::{AppState, CachedWarController, PreSerializedEvent};

pub async fn run(state: AppState) {
    let Some(url) = warcontroller_url() else {
        info!("SEQUOIA_BACKEND_BASE_URL is unset; war controller poller disabled");
        return;
    };

    let token = sequoia_backend_internal_token();
    let mut interval = tokio::time::interval(warcontroller_poll_interval());
    info!(%url, "war controller poller started");

    loop {
        interval.tick().await;
        poll_once(&state, &url, token.as_deref()).await;
    }
}

/// Fetch once, caching and broadcasting only when the state actually changed.
/// Returns `true` when a broadcast was emitted.
async fn poll_once(state: &AppState, url: &str, token: Option<&str>) -> bool {
    let fetched = match fetch_warcontroller(&state.http_client, url, token).await {
        Ok(fetched) => fetched,
        Err(e) => {
            warn!("failed to fetch war controller state; keeping cached value: {e}");
            return false;
        }
    };

    let unchanged = state
        .warcontroller_cache
        .read()
        .await
        .as_ref()
        .is_some_and(|cached| cached.state == fetched);
    if unchanged {
        debug!("war controller state unchanged; skipping broadcast");
        return false;
    }

    let json = match serde_json::to_vec(&fetched) {
        Ok(bytes) => Arc::new(Bytes::from(bytes)),
        Err(e) => {
            warn!("failed to serialize war controller state: {e}");
            return false;
        }
    };

    *state.warcontroller_cache.write().await = Some(CachedWarController {
        state: fetched,
        json: json.clone(),
    });

    // A send error only means nobody is subscribed right now.
    let _ = state
        .event_tx
        .send(PreSerializedEvent::WarController { json });
    true
}

async fn fetch_warcontroller(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<WarControllerState, String> {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("failed to read response body: {e}"))?;

    if !status.is_success() {
        let preview = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>();
        return Err(format!("upstream status {status}; body preview: {preview}"));
    }

    serde_json::from_slice(&bytes).map_err(|e| format!("failed to parse response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    const TOKEN: &str = "test-warcontroller-token-0123456789";

    /// Payload mirroring the backend sample, including its `pos.x` number /
    /// `pos.z` string inconsistency.
    fn payload(health: f64) -> String {
        format!(
            r#"{{"timestamp": 1787517420,
                "queues": [{{"territory": "Entrance to Olux", "difficulty": "VERY_HIGH", "status": "STARTED", "timestamp": 1787517443}}],
                "wars": [{{"territory": "Entrance to Olux", "difficulty": "VERY_HIGH", "health": {health}, "start": 1787517417, "ehp": 24135275, "dps": 32143}}],
                "players": [{{"username": "Yearnm", "class": "MAGE", "territory": null, "pos": {{"x": -1517, "z": "-5130"}}}}]}}"#
        )
    }

    /// Serves `payload` with a decreasing health value, so successive polls differ only
    /// when the test wants them to. Requires the bearer token, like the real backend.
    async fn spawn_backend(healths: Vec<f64>) -> String {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = move |headers: HeaderMap| {
            let calls = Arc::clone(&calls);
            let healths = healths.clone();
            async move {
                let presented = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "));
                if presented != Some(TOKEN) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                let index = calls.fetch_add(1, Ordering::SeqCst).min(healths.len() - 1);
                Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(payload(healths[index])))
                    .expect("response builds")
            }
        };

        let app = Router::new().route("/internal/warcontroller", get(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds an ephemeral port");
        let addr = listener.local_addr().expect("has a local address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/internal/warcontroller")
    }

    #[tokio::test]
    async fn caches_and_broadcasts_then_skips_unchanged_state() {
        // Same value twice, then a different one.
        let url = spawn_backend(vec![0.8731, 0.8731, 0.5]).await;
        let state = AppState::new(None);
        let mut rx = state.event_tx.subscribe();

        assert!(
            poll_once(&state, &url, Some(TOKEN)).await,
            "first poll should cache and broadcast"
        );

        {
            let cached = state.warcontroller_cache.read().await;
            let cached = cached.as_ref().expect("state is cached");
            assert_eq!(cached.state.wars.len(), 1);
            assert_eq!(cached.state.wars[0].ehp, Some(24_135_275));
            // The lenient position deserializer handled the string-encoded z.
            let pos = cached.state.players[0]
                .pos
                .expect("roaming player has a pos");
            assert_eq!((pos.x, pos.z), (-1517.0, -5130.0));
        }

        match rx.try_recv() {
            Ok(PreSerializedEvent::WarController { json }) => {
                let raw = std::str::from_utf8(json.as_ref()).expect("payload is utf-8");
                assert!(raw.contains("Entrance to Olux"));
            }
            other => panic!("expected a WarController broadcast, got {other:?}"),
        }

        assert!(
            !poll_once(&state, &url, Some(TOKEN)).await,
            "identical state should not rebroadcast"
        );
        assert!(rx.try_recv().is_err(), "no event for unchanged state");

        assert!(
            poll_once(&state, &url, Some(TOKEN)).await,
            "changed state should broadcast again"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(PreSerializedEvent::WarController { .. })
        ));
    }

    #[tokio::test]
    async fn keeps_cached_state_when_the_backend_rejects_the_token() {
        let url = spawn_backend(vec![0.8731]).await;
        let state = AppState::new(None);

        assert!(
            !poll_once(&state, &url, Some("wrong-token")).await,
            "a 401 must not cache or broadcast"
        );
        assert!(state.warcontroller_cache.read().await.is_none());
    }
}
