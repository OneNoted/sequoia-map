use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::Sse;
use axum::response::sse::{Event, KeepAlive};
use bytes::Bytes;
use futures::stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;

use crate::config::SSE_KEEPALIVE_SECS;
use crate::state::{AppState, PreSerializedEvent};

pub async fn territory_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        // Send pre-serialized snapshot (Arc clone = O(1) refcount bump, not 200KB String copy)
        let (seq, data) = {
            let snapshot = state.live_snapshot.read().await;
            (snapshot.seq, snapshot.snapshot_json.clone())
        };
        if !data.is_empty() {
            if let Some(payload) = event_payload(data.as_ref()) {
                yield Ok(
                    Event::default()
                        .id(seq.to_string())
                        .event("snapshot")
                        .data(payload),
                );
            } else {
                warn!("snapshot payload is not valid utf-8; skipping SSE snapshot event");
            }
        }

        // Subscribe to updates
        let rx = state.event_tx.subscribe();
        let mut stream = BroadcastStream::new(rx);

        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    let (event_type, seq, data) = match event {
                        PreSerializedEvent::Snapshot { seq, json } => ("snapshot", seq, json),
                        PreSerializedEvent::Update { seq, json } => ("update", seq, json),
                        PreSerializedEvent::RuntimeUpdate { seq, json } => {
                            ("runtime_update", seq, json)
                        }
                    };
                    let Some(payload) = event_payload(data.as_ref()) else {
                        warn!(
                            seq,
                            event = event_type,
                            "event payload is not valid utf-8; dropping SSE event"
                        );
                        continue;
                    };
                    yield Ok(
                        Event::default()
                            .id(seq.to_string())
                            .event(event_type)
                            .data(payload),
                    );
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

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(SSE_KEEPALIVE_SECS))
            .text("keep-alive"),
    )
}

fn event_payload(bytes: &Bytes) -> Option<&str> {
    std::str::from_utf8(bytes.as_ref()).ok()
}
