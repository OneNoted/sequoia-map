use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use bytes::Bytes;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;

use crate::config::SSE_KEEPALIVE_SECS;
use crate::state::{AppState, PreSerializedEvent};

pub async fn territory_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let stream = async_stream::stream! {
        // Send pre-serialized snapshot (Arc clone = O(1) refcount bump, not 200KB String copy)
        let (seq, data) = {
            let snapshot = state.live_snapshot.read().await;
            (snapshot.seq, snapshot.snapshot_json.clone())
        };
        if !data.is_empty() {
            if let Some(payload) = event_payload(data.as_ref()) {
                // Pins the stream's error type; the handler now returns an erased
                // `Response`, so there is no signature left to infer it from.
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .id(seq.to_string())
                        .event("snapshot")
                        .data(payload),
                );
            } else {
                warn!("snapshot payload is not valid utf-8; skipping SSE snapshot event");
            }
        }

        // The war feed is internal guild intel: resolve the session before emitting any of
        // it, and never for a viewer outside Sequoia. Deliberately after the snapshot -
        // the probe can take seconds, and the map's first paint must not wait on it.
        let war_feed_visible = crate::routes::auth::viewer_is_guild_member(
            crate::routes::auth::resolve_viewer(&state, &headers).await.as_ref(),
        );

        // Seed war controller state so a fresh client isn't blank until the next poll tick.
        let warcontroller = if war_feed_visible {
            state
                .warcontroller_cache
                .read()
                .await
                .as_ref()
                .map(|cached| cached.json.clone())
        } else {
            None
        };
        if let Some(data) = warcontroller {
            if let Some(payload) = event_payload(data.as_ref()) {
                yield Ok(Event::default().event("warcontroller").data(payload));
            } else {
                warn!("warcontroller payload is not valid utf-8; skipping SSE seed event");
            }
        }

        // Subscribe to updates
        let rx = state.event_tx.subscribe();
        let mut stream = BroadcastStream::new(rx);

        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    // `seq` is None for events outside the territory sequence stream; those
                    // are emitted without an SSE id so Last-Event-ID reconnect stays correct.
                    let (event_type, seq, data) = match event {
                        PreSerializedEvent::Snapshot { seq, json } => ("snapshot", Some(seq), json),
                        PreSerializedEvent::Update { seq, json } => ("update", Some(seq), json),
                        PreSerializedEvent::RuntimeUpdate { seq, json } => {
                            ("runtime_update", Some(seq), json)
                        }
                        PreSerializedEvent::WarController { json } => {
                            if !war_feed_visible {
                                continue;
                            }
                            ("warcontroller", None, json)
                        }
                    };
                    let Some(payload) = event_payload(data.as_ref()) else {
                        warn!(
                            ?seq,
                            event = event_type,
                            "event payload is not valid utf-8; dropping SSE event"
                        );
                        continue;
                    };
                    let mut sse_event = Event::default().event(event_type).data(payload);
                    if let Some(seq) = seq {
                        sse_event = sse_event.id(seq.to_string());
                    }
                    yield Ok(sse_event);
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                    warn!(
                        skipped_events = skipped,
                        "SSE client lagged behind broadcast buffer; replaying snapshot"
                    );
                    // Client fell behind — resend pre-serialized snapshot (Arc clone = O(1))
                    let (seq, data) = {
                        let snapshot = state.live_snapshot.read().await;
                        (snapshot.seq, snapshot.snapshot_json.clone())
                    };
                    if !data.is_empty() {
                        let Some(payload) = event_payload(data.as_ref()) else {
                            warn!("snapshot payload is not valid utf-8; skipping SSE snapshot replay");
                            continue;
                        };
                        yield Ok(
                            Event::default()
                                .id(seq.to_string())
                                .event("snapshot")
                                .data(payload),
                        );
                    }
                }
            }
        }
    };

    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(SSE_KEEPALIVE_SECS))
                .text("keep-alive"),
        )
        .into_response();
    // The stream's war controller frames depend on the session, so this body must never
    // be cached or shared between viewers.
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response_headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
    response
}

fn event_payload(bytes: &Bytes) -> Option<&str> {
    std::str::from_utf8(bytes.as_ref()).ok()
}
