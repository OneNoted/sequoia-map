use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use sequoia_shared::WarControllerState;
use tracing::{debug, info, warn};

use crate::config::{
    sequoia_backend_internal_token, warcontroller_max_staleness, warcontroller_poll_interval,
    warcontroller_url,
};
use crate::state::{AppState, CachedWarController, PreSerializedEvent};

pub async fn run(state: AppState) {
    let Some(url) = warcontroller_url() else {
        info!("SEQUOIA_BACKEND_BASE_URL is unset; war controller poller disabled");
        return;
    };

    let token = sequoia_backend_internal_token();
    if token.is_none() && url.contains("/internal/") {
        // The backend enforces the bearer token on the `/internal/` prefix, so this
        // configuration can only ever produce 401s. Say so once instead of letting the
        // per-poll warnings look like an upstream outage.
        warn!(
            %url,
            "SEQUOIA_BACKEND_INTERNAL_TOKEN is unset or too short; the war controller feed will \
             be refused. Set the token, or point SEQUOIA_BACKEND_WARCONTROLLER_PATH at an \
             unauthenticated alias."
        );
    }
    let mut interval = tokio::time::interval(warcontroller_poll_interval());
    // Read once at startup, like the poll interval: a knob that changes mid-process would
    // make the cache's own age mean two different things.
    let max_staleness = warcontroller_max_staleness();
    info!(%url, "war controller poller started");

    loop {
        interval.tick().await;
        poll_once(&state, &url, token.as_deref(), max_staleness).await;
    }
}

/// Fetch once, caching and broadcasting only when the state actually changed.
///
/// A failed fetch keeps the cached payload until it exceeds [`warcontroller_max_staleness`],
/// then drops it and broadcasts an empty state - see [`expire_stale_cache`].
/// Returns `true` when a broadcast was emitted.
async fn poll_once(
    state: &AppState,
    url: &str,
    token: Option<&str>,
    max_staleness: Option<Duration>,
) -> bool {
    let fetched = match fetch_warcontroller(&state.http_client, url, token).await {
        Ok(fetched) => fetched,
        Err(e) => {
            return expire_stale_cache(state, max_staleness, &e).await;
        }
    };

    // Compared on content alone: the backend stamps every response afresh, so an equality
    // test that included the timestamp would never hold and this guard would never fire.
    let unchanged = state
        .warcontroller_cache
        .read()
        .await
        .as_ref()
        .is_some_and(|cached| cached.state.same_content(&fetched));
    if unchanged {
        debug!("war controller state unchanged; skipping broadcast");
        // The poll still succeeded, so the cache is confirmed current. Without this, a feed
        // that simply has no wars in it would age past `max_staleness` and be dropped by the
        // first failed poll after that, as though the backend had been away all along.
        if let Some(cached) = state.warcontroller_cache.write().await.as_mut() {
            cached.fetched_at = Utc::now();
        }
        return false;
    }

    let json = match serde_json::to_vec(&fetched) {
        Ok(bytes) => Arc::new(Bytes::from(bytes)),
        Err(e) => {
            warn!("failed to serialize war controller state: {e}");
            return false;
        }
    };

    let timestamp = fetched.timestamp;
    *state.warcontroller_cache.write().await = Some(CachedWarController {
        state: fetched,
        json: json.clone(),
        fetched_at: Utc::now(),
    });

    // A send error only means nobody is subscribed right now.
    let _ = state.event_tx.send(PreSerializedEvent::WarController {
        timestamp,
        clears: false,
        json,
    });
    true
}

/// Handles a failed poll: keep the cached payload while it is still plausibly current, drop it
/// once it is not.
///
/// Returns `true` when the drop was broadcast. Without the bound, a backend that goes away
/// mid-war leaves every client - and `/api/warcontroller` - presenting that war as live
/// indefinitely, ETA pinned at zero, with no later event to correct it.
async fn expire_stale_cache(
    state: &AppState,
    max_staleness: Option<Duration>,
    error: &str,
) -> bool {
    let Some(max_staleness) = max_staleness else {
        warn!("failed to fetch war controller state; keeping cached value: {error}");
        return false;
    };

    let age = {
        let cache = state.warcontroller_cache.read().await;
        match cache.as_ref() {
            Some(cached) => Utc::now().signed_duration_since(cached.fetched_at),
            // Nothing cached, so nothing to go stale.
            None => {
                warn!("failed to fetch war controller state: {error}");
                return false;
            }
        }
    };
    // A negative age means the clock stepped backwards, not that the cache is ancient; treat
    // it as fresh and let the next poll decide.
    let stale = chrono::Duration::from_std(max_staleness).is_ok_and(|limit| age >= limit);
    if !stale {
        warn!("failed to fetch war controller state; keeping cached value: {error}");
        return false;
    }

    let empty = WarControllerState {
        timestamp: Utc::now().timestamp(),
        queues: Vec::new(),
        wars: Vec::new(),
        players: Vec::new(),
    };
    let json = match serde_json::to_vec(&empty) {
        Ok(bytes) => Arc::new(Bytes::from(bytes)),
        Err(e) => {
            warn!("failed to serialize the empty war controller state: {e}");
            return false;
        }
    };
    warn!(
        stale_secs = age.num_seconds(),
        "war controller state is stale and the backend is unreachable; dropping it: {error}"
    );
    *state.warcontroller_cache.write().await = None;
    let _ = state.event_tx.send(PreSerializedEvent::WarController {
        timestamp: empty.timestamp,
        // Stamped on our clock, not the backend's: receivers must not order it against the
        // backend-stamped frame it is dropping.
        clears: true,
        json,
    });
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
    /// A port nothing listens on, so every fetch against it fails.
    const UNREACHABLE: &str = "http://127.0.0.1:1/internal/warcontroller";

    /// Payload mirroring the backend sample, including its `pos.x` number /
    /// `pos.z` string inconsistency.
    fn payload(health: f64) -> String {
        payload_at(1_787_517_420, health)
    }

    /// A live backend restamps every response, so the fake one must too - otherwise the
    /// unchanged-state guard is tested against a shape it never sees in production.
    fn payload_at(timestamp: i64, health: f64) -> String {
        format!(
            r#"{{"timestamp": {timestamp},
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
                let call = calls.fetch_add(1, Ordering::SeqCst);
                let index = call.min(healths.len() - 1);
                Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    // A fresh stamp on every response, exactly like the real backend.
                    .body(axum::body::Body::from(payload_at(
                        1_787_517_420 + call as i64,
                        healths[index],
                    )))
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
            poll_once(&state, &url, Some(TOKEN), None).await,
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
            Ok(PreSerializedEvent::WarController { clears, json, .. }) => {
                let raw = std::str::from_utf8(json.as_ref()).expect("payload is utf-8");
                assert!(raw.contains("Entrance to Olux"));
                assert!(
                    !clears,
                    "a polled frame carries the backend's clock and stays under the \
                     receiver's monotonic guard"
                );
            }
            other => panic!("expected a WarController broadcast, got {other:?}"),
        }

        assert!(
            !poll_once(&state, &url, Some(TOKEN), None).await,
            "identical state should not rebroadcast"
        );
        assert!(rx.try_recv().is_err(), "no event for unchanged state");

        assert!(
            poll_once(&state, &url, Some(TOKEN), None).await,
            "changed state should broadcast again"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(PreSerializedEvent::WarController { .. })
        ));
    }

    #[tokio::test]
    async fn an_unchanged_poll_still_marks_the_cache_as_confirmed() {
        // Same wars twice. The second poll broadcasts nothing, but it did reach the backend,
        // so the cache must not keep ageing towards expiry.
        let url = spawn_backend(vec![0.8731, 0.8731]).await;
        let state = AppState::new(None);

        assert!(poll_once(&state, &url, Some(TOKEN), None).await);
        {
            let mut cache = state.warcontroller_cache.write().await;
            cache.as_mut().expect("state is cached").fetched_at =
                Utc::now() - chrono::Duration::seconds(600);
        }

        assert!(
            !poll_once(&state, &url, Some(TOKEN), None).await,
            "unchanged content must not rebroadcast"
        );

        let cache = state.warcontroller_cache.read().await;
        let age =
            Utc::now().signed_duration_since(cache.as_ref().expect("still cached").fetched_at);
        assert!(
            age < chrono::Duration::seconds(5),
            "a successful poll refreshes the cache's age, got {age}"
        );
    }

    #[tokio::test]
    async fn keeps_cached_state_when_the_backend_rejects_the_token() {
        let url = spawn_backend(vec![0.8731]).await;
        let state = AppState::new(None);

        assert!(
            !poll_once(&state, &url, Some("wrong-token"), None).await,
            "a 401 must not cache or broadcast"
        );
        assert!(state.warcontroller_cache.read().await.is_none());
    }

    /// Seeds the cache as if a poll had succeeded `age` ago.
    async fn seed_cache(state: &AppState, age: chrono::Duration) {
        let raw = payload(0.8731);
        let parsed: WarControllerState = serde_json::from_str(&raw).expect("payload parses");
        *state.warcontroller_cache.write().await = Some(CachedWarController {
            state: parsed,
            json: Arc::new(Bytes::from(raw.into_bytes())),
            fetched_at: Utc::now() - age,
        });
    }

    #[tokio::test]
    async fn a_brief_outage_keeps_the_cached_state() {
        // Nothing to serve the request, so every poll fails.
        let state = AppState::new(None);
        seed_cache(&state, chrono::Duration::seconds(5)).await;
        let mut rx = state.event_tx.subscribe();

        assert!(
            !poll_once(&state, UNREACHABLE, None, Some(Duration::from_secs(60))).await,
            "a fresh cache must survive a failed poll"
        );

        assert!(state.warcontroller_cache.read().await.is_some());
        assert!(rx.try_recv().is_err(), "nothing to tell clients yet");
    }

    #[tokio::test]
    async fn a_long_outage_drops_the_cache_and_broadcasts_an_empty_state() {
        let state = AppState::new(None);
        seed_cache(&state, chrono::Duration::seconds(120)).await;
        let mut rx = state.event_tx.subscribe();

        assert!(
            poll_once(&state, UNREACHABLE, None, Some(Duration::from_secs(60))).await,
            "a stale cache must be dropped and the drop broadcast"
        );

        assert!(
            state.warcontroller_cache.read().await.is_none(),
            "a war that ended during the outage must not stay cached"
        );
        match rx.try_recv() {
            Ok(PreSerializedEvent::WarController {
                timestamp,
                clears,
                json,
            }) => {
                let cleared: WarControllerState =
                    serde_json::from_slice(json.as_ref()).expect("cleared payload parses");
                assert!(cleared.wars.is_empty() && cleared.queues.is_empty());
                assert!(
                    clears,
                    "the drop is stamped on our clock, so receivers must not order it \
                     against the backend-stamped frame it replaces"
                );
                // Stamped now, so it wins the client's monotonic check against the state it
                // is clearing.
                assert_eq!(timestamp, cleared.timestamp);
                assert!(timestamp > 1_787_517_420);
            }
            other => panic!("expected a cleared WarController broadcast, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn expiry_can_be_disabled() {
        let state = AppState::new(None);
        seed_cache(&state, chrono::Duration::days(1)).await;

        // `None` is what `warcontroller_max_staleness` returns for `0`.
        assert!(!poll_once(&state, UNREACHABLE, None, None).await);

        assert!(state.warcontroller_cache.read().await.is_some());
    }
}
