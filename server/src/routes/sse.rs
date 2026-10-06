use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use bytes::Bytes;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;

use crate::config::SSE_KEEPALIVE_SECS;
use crate::routes::auth::ViewerProbe;
use crate::state::{AppState, PreSerializedEvent};

/// How often a long-lived stream re-checks that its viewer is still a Sequoia member.
///
/// The cookie was read once at connect and cannot be refreshed here, so re-probing it is what
/// catches an expired session or a revoked guild rank; without it a stream that never drops
/// keeps serving guild-internal war intel indefinitely. Checked only when a war frame is
/// actually pending, so an idle stream costs nothing.
const WAR_FEED_REVALIDATE: Duration = Duration::from_secs(300);

/// How soon to re-probe after the backend failed to answer at all.
///
/// Shorter than [`WAR_FEED_REVALIDATE`] so a stream that connected during a restart, or that
/// is riding out one, recovers within seconds of the backend coming back.
const WAR_FEED_RETRY: Duration = Duration::from_secs(30);

/// How long a stream keeps serving while the probe stays unanswerable.
///
/// A failed probe is not a verdict, so muting on the first one would let a three-second
/// backend restart strip a member's war panel. Serving *forever* would let a revocation
/// outlive a long outage, hence the bound - after which the stream stops serving but keeps
/// re-probing, so it recovers on its own once the backend is back.
const WAR_FEED_UNKNOWN_GRACE: Duration = Duration::from_secs(300);

/// What a stream is currently allowed to do with the war feed.
///
/// The point of the third variant is that "not a member" and "we could not tell" are not the
/// same thing and must not be stored the same way: the first is final, the second is a
/// blackout to be ridden out and re-probed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarFeedAccess {
    /// A confirmed Sequoia member.
    Granted,
    /// A confirmed non-member, or a session the backend refused. The only settled state, and
    /// never re-probed - so an anonymous stream costs the backend one round trip for its
    /// whole life.
    Denied,
    /// The probe could not be answered. `serving` carries the decision being held over, and
    /// is false for a stream that never had one.
    Unknown { serving: bool, since: Instant },
}

impl WarFeedAccess {
    /// Whether war frames may go out right now.
    fn serves(self) -> bool {
        matches!(self, Self::Granted | Self::Unknown { serving: true, .. })
    }

    /// Whether this stream should keep spending backend round trips on re-probing.
    fn is_final(self) -> bool {
        matches!(self, Self::Denied)
    }
}

/// Folds a fresh probe into the current access state.
///
/// Kept as a free function so the policy - hold on failure, give up after
/// [`WAR_FEED_UNKNOWN_GRACE`], answer immediately to anything conclusive - is testable
/// without a live stream. `now` is passed in for the same reason.
fn next_access(current: WarFeedAccess, probe: &ViewerProbe, now: Instant) -> WarFeedAccess {
    if !probe.is_unavailable() {
        return if crate::routes::auth::viewer_is_guild_member(probe.viewer()) {
            WarFeedAccess::Granted
        } else {
            WarFeedAccess::Denied
        };
    }
    match current {
        // The blackout starts now, carrying the decision it interrupted.
        WarFeedAccess::Granted => WarFeedAccess::Unknown {
            serving: true,
            since: now,
        },
        WarFeedAccess::Unknown { serving, since } => {
            // The grace window stops the *serving*, not the probing. Settling on `Denied`
            // here would mute the stream for good over an outage that has said nothing about
            // this viewer - the same permanent muting this state exists to prevent, just five
            // minutes later. Restart the window instead, and keep asking.
            if serving && now.duration_since(since) >= WAR_FEED_UNKNOWN_GRACE {
                WarFeedAccess::Unknown {
                    serving: false,
                    since: now,
                }
            } else {
                WarFeedAccess::Unknown { serving, since }
            }
        }
        WarFeedAccess::Denied => WarFeedAccess::Denied,
    }
}

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

        // Subscribe before the war feed is read. The membership probe below can take seconds,
        // and a cache change landing in that window would otherwise reach no receiver: the
        // client would keep the older seed with no later event to repair it.
        let rx = state.event_tx.subscribe();
        let mut stream = BroadcastStream::new(rx);

        // The war feed is internal guild intel: resolve the session before emitting any of
        // it, and never for a viewer outside Sequoia. Deliberately after the snapshot -
        // the probe can take seconds, and the map's first paint must not wait on it.
        let probe = crate::routes::auth::resolve_viewer(&state, &headers).await;
        let now = Instant::now();
        // A connect-time failure starts as a blackout that has served nothing: no war data
        // goes out yet, but the stream re-probes and opens up on its own once the backend is
        // back, rather than staying blank until the viewer reloads the page.
        let mut access = next_access(
            WarFeedAccess::Unknown { serving: false, since: now },
            &probe,
            now,
        );
        let mut next_probe_due = now
            + if probe.is_unavailable() {
                WAR_FEED_RETRY
            } else {
                WAR_FEED_REVALIDATE
            };
        // Newest war payload this stream has emitted. Subscribing first means a live frame can
        // now be queued *behind* an older seed or lag replay, so every war frame is checked
        // against this and dropped if it would move the client backwards.
        let mut last_war_ts: Option<i64> = None;

        // Seed war controller state so a fresh client isn't blank until the next poll tick.
        let warcontroller = if access.serves() {
            state
                .warcontroller_cache
                .read()
                .await
                .as_ref()
                .map(|cached| (cached.state.timestamp, cached.json.clone()))
        } else {
            None
        };
        match warcontroller {
            Some((timestamp, data)) => {
                if let Some(payload) = event_payload(data.as_ref()) {
                    last_war_ts = Some(timestamp);
                    yield Ok(Event::default().event("warcontroller").data(payload));
                } else {
                    warn!("warcontroller payload is not valid utf-8; skipping SSE seed event");
                }
            }
            // Nothing cached means the poller dropped a stale payload while the backend was
            // away. Say so explicitly: a client that reconnects mid-outage would otherwise
            // keep rendering the wars it held from before it, with no later event to correct
            // them. The watermark is deliberately left unset, so the first real frame is
            // accepted even if the backend stamps it a moment before this one - and the
            // client is covered by the same rule from the other side, since
            // `WarControllerState::supersedes` lets real data beat a held clear outright.
            None if access.serves() => {
                if let Some(payload) = empty_war_state() {
                    yield Ok(Event::default().event("warcontroller").data(payload));
                }
            }
            None => {}
        }

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
                        PreSerializedEvent::WarController { timestamp, clears, json } => {
                            // A settled denial is the end of it; anything else is re-probed,
                            // which is what lets a stream that connected during an outage
                            // start serving once the backend answers again.
                            if access.is_final() {
                                continue;
                            }
                            if Instant::now() >= next_probe_due {
                                let probe = crate::routes::auth::resolve_viewer(&state, &headers)
                                    .await;
                                let now = Instant::now();
                                let was_serving = access.serves();
                                access = next_access(access, &probe, now);
                                next_probe_due = now
                                    + if probe.is_unavailable() {
                                        WAR_FEED_RETRY
                                    } else {
                                        WAR_FEED_REVALIDATE
                                    };
                                if was_serving && !access.serves() {
                                    if probe.is_unavailable() {
                                        warn!(
                                            "the website session could not be re-probed for \
                                             {}s; clearing the war feed for this stream until \
                                             the backend answers again",
                                            WAR_FEED_UNKNOWN_GRACE.as_secs()
                                        );
                                    } else {
                                        warn!(
                                            "war feed viewer is no longer a Sequoia member; \
                                             clearing and muting the feed for this stream"
                                        );
                                    }
                                    // One last frame so the panel and the at-war highlight
                                    // clear on the client, rather than freezing on whatever
                                    // it was last shown. The territory stream continues.
                                    // The watermark is left unset, as it is for the empty
                                    // seed above: this clear is stamped on *our* clock, and
                                    // pinning it would drop the first frame after a re-grant
                                    // if the backend's clock trails ours. Nothing stale can
                                    // slip through in the meantime - frames are skipped
                                    // outright while the feed is muted.
                                    if let Some(cleared) = empty_war_state() {
                                        last_war_ts = None;
                                        yield Ok(
                                            Event::default()
                                                .event("warcontroller")
                                                .data(cleared),
                                        );
                                    }
                                }
                            }
                            if !access.serves() {
                                continue;
                            }
                            if !admit_war_frame(&mut last_war_ts, timestamp, clears) {
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

                    // War frames ride the same channel, so they are among the events a lagging
                    // client just missed. If the skipped one was the final "war ended", nothing
                    // else would ever correct it - replay the current cache too.
                    let replay = if access.serves() {
                        state
                            .warcontroller_cache
                            .read()
                            .await
                            .as_ref()
                            .map(|cached| (cached.state.timestamp, cached.json.clone()))
                    } else {
                        None
                    };
                    match replay {
                        Some((timestamp, data)) => {
                            if !accept_war_frame(&mut last_war_ts, timestamp) {
                                continue;
                            }
                            let Some(payload) = event_payload(data.as_ref()) else {
                                warn!("warcontroller payload is not valid utf-8; skipping SSE replay");
                                continue;
                            };
                            yield Ok(Event::default().event("warcontroller").data(payload));
                        }
                        // An empty cache is a state in its own right, and the broadcast that
                        // announced it is one of the events this client just missed. Replaying
                        // nothing here would leave it rendering the expired war for good, which
                        // is exactly what the staleness bound exists to prevent. Same rule as
                        // the empty seed: emitted unconditionally, watermark left unset.
                        None if access.serves() => {
                            if let Some(payload) = empty_war_state() {
                                last_war_ts = None;
                                yield Ok(Event::default().event("warcontroller").data(payload));
                            }
                        }
                        None => {}
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

/// Whether a broadcast war frame goes out on this stream, and what it does to the watermark.
///
/// A frame this server generated to *clear* the feed always goes out, and releases the
/// watermark instead of setting it. Every real frame is stamped by the backend while a clear
/// is stamped here, so the two are not comparable: ordering them would drop the clear whenever
/// the backend's clock leads ours - stranding every connected client on a war whose cache was
/// deliberately dropped - and adopting its stamp would drop the backend's first frame after
/// recovery whenever the backend's clock trails ours. The empty seed and the revocation frame
/// release the watermark for the same reason, and `WarControllerState::supersedes` applies the
/// matching rule on the client.
fn admit_war_frame(last_war_ts: &mut Option<i64>, timestamp: i64, clears: bool) -> bool {
    if clears {
        *last_war_ts = None;
        return true;
    }
    accept_war_frame(last_war_ts, timestamp)
}

/// Whether a war frame stamped `timestamp` may be emitted, recording it when it may.
///
/// Guards against moving a client backwards: seeds, lag replays and live frames are read from
/// three different points and can interleave. Equal timestamps pass - the poller only
/// broadcasts on a real change, so a repeat is a re-seed of the same state, not a regression.
fn accept_war_frame(last_war_ts: &mut Option<i64>, timestamp: i64) -> bool {
    if last_war_ts.is_some_and(|last| timestamp < last) {
        return false;
    }
    *last_war_ts = Some(timestamp);
    true
}

/// An empty war state stamped now, with its timestamp, for a stream that has nothing to show.
///
/// No caller - the empty seed, the empty lag replay, or a revocation - records the timestamp
/// as a watermark.
/// A clear is stamped on *this* server's clock while every real frame is stamped by the
/// backend, so pinning the watermark here would drop a recovered backend's first frame
/// whenever that backend's clock trails ours. `WarControllerState::supersedes` applies the
/// matching rule on the client, which holds the clear under the same skew.
///
/// Stamped rather than zeroed so it survives the client's own monotonic check; `None` only if
/// serialization fails, which cannot happen for this shape.
fn empty_war_state() -> Option<String> {
    let cleared = sequoia_shared::WarControllerState {
        timestamp: chrono::Utc::now().timestamp(),
        queues: Vec::new(),
        wars: Vec::new(),
        players: Vec::new(),
    };
    serde_json::to_string(&cleared)
        .inspect_err(|e| warn!("failed to serialize the cleared war controller state: {e}"))
        .ok()
}

fn event_payload(bytes: &Bytes) -> Option<&str> {
    std::str::from_utf8(bytes.as_ref()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_war_frame_is_always_accepted() {
        let mut last = None;
        assert!(accept_war_frame(&mut last, 1_787_517_420));
        assert_eq!(last, Some(1_787_517_420));
    }

    #[test]
    fn a_stale_frame_is_dropped_without_moving_the_watermark() {
        // A live frame queued behind a newer seed, or a lag replay of an older cache.
        let mut last = Some(1_000);
        assert!(!accept_war_frame(&mut last, 999));
        assert_eq!(last, Some(1_000));
    }

    #[test]
    fn a_repeat_of_the_current_state_still_goes_out() {
        // The poller broadcasts only on a real change, so an equal timestamp is a re-seed.
        let mut last = Some(1_000);
        assert!(accept_war_frame(&mut last, 1_000));
        assert!(accept_war_frame(&mut last, 1_001));
        assert_eq!(last, Some(1_001));
    }

    #[test]
    fn the_revocation_frame_clears_the_feed_and_outranks_what_it_replaces() {
        let payload = empty_war_state().expect("the cleared state serializes");

        let cleared: sequoia_shared::WarControllerState =
            serde_json::from_str(&payload).expect("cleared payload parses");
        assert!(cleared.is_empty());
        // Stamped now, so neither this stream's own guard nor the client's rejects it.
        assert!(cleared.timestamp > 1_787_517_420);
        let mut last = Some(1_787_517_420);
        assert!(accept_war_frame(&mut last, cleared.timestamp));
    }

    #[test]
    fn a_clear_is_never_ordered_against_a_backend_stamped_frame() {
        // The staleness expiry stamps the drop on this server's clock. A backend running a
        // few seconds ahead used to make that drop lose the comparison, and every stream
        // held the expired war until the backend came back with something newer.
        let mut last = Some(1_787_517_420);
        assert!(
            admit_war_frame(&mut last, 1_787_517_400, true),
            "a clear must go out however the clocks compare"
        );
        assert_eq!(last, None, "and must release the watermark, not set it");

        // The backend's first frame after recovery is then accepted even if it trails.
        assert!(admit_war_frame(&mut last, 1_787_517_390, false));
        assert_eq!(last, Some(1_787_517_390));
    }

    #[test]
    fn an_empty_frame_the_backend_sent_is_still_ordinary_data() {
        // Only a locally generated clear is exempt; a war ending upstream is a normal frame
        // carrying the backend's own clock, and stays under the monotonic guard.
        let mut last = Some(1_000);
        assert!(!admit_war_frame(&mut last, 999, false));
        assert_eq!(last, Some(1_000));
    }

    #[test]
    fn a_clear_never_becomes_a_watermark_that_outlives_it() {
        // Both callers drop the stamp on the floor: a clear is measured on this server's
        // clock, and adopting it would drop the first frame a backend running slightly
        // behind us sends once it recovers. The client holds the matching rule.
        let cleared: sequoia_shared::WarControllerState =
            serde_json::from_str(&empty_war_state().expect("the cleared state serializes"))
                .expect("cleared payload parses");
        let recovered = sequoia_shared::WarControllerState {
            timestamp: cleared.timestamp - 5,
            queues: Vec::new(),
            wars: vec![sequoia_shared::ActiveWar {
                territory: "Entrance to Olux".to_string(),
                difficulty: Some("VERY_HIGH".to_string()),
                health: 0.5,
                start: cleared.timestamp - 60,
                ehp: None,
                dps: None,
            }],
            players: Vec::new(),
        };

        // Server side: no watermark, so the lagging frame goes out.
        let mut last = None;
        assert!(accept_war_frame(&mut last, recovered.timestamp));
        // Client side: the held clear yields to it despite the older stamp.
        assert!(recovered.supersedes(Some(&cleared)));
    }

    fn viewer_probe(guild_rank: Option<&str>) -> ViewerProbe {
        ViewerProbe::Viewer(crate::routes::auth::Viewer {
            discord_id: "1".to_string(),
            discord_username: None,
            minecraft_uuid: "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b".to_string(),
            minecraft_username: None,
            website_admin: false,
            guild_rank: guild_rank.map(str::to_string),
        })
    }

    #[test]
    fn a_conclusive_probe_settles_access_either_way() {
        let now = Instant::now();
        let fresh = WarFeedAccess::Unknown {
            serving: false,
            since: now,
        };

        assert_eq!(
            next_access(fresh, &viewer_probe(Some("chief")), now),
            WarFeedAccess::Granted
        );
        assert_eq!(
            next_access(fresh, &viewer_probe(None), now),
            WarFeedAccess::Denied
        );
        assert_eq!(
            next_access(WarFeedAccess::Granted, &ViewerProbe::Anonymous, now),
            WarFeedAccess::Denied
        );
    }

    #[test]
    fn a_failed_probe_holds_a_members_feed_open() {
        // The blip this whole state exists for: the backend restarts, and the member keeps
        // their panel instead of losing it for the life of the page.
        let now = Instant::now();
        let held = next_access(WarFeedAccess::Granted, &ViewerProbe::Unavailable, now);
        assert_eq!(
            held,
            WarFeedAccess::Unknown {
                serving: true,
                since: now
            }
        );
        assert!(held.serves());
        assert!(!held.is_final());

        // Still failing, but well inside the grace window.
        let later = now + WAR_FEED_UNKNOWN_GRACE / 2;
        let still_held = next_access(held, &ViewerProbe::Unavailable, later);
        assert_eq!(still_held, held, "the blackout keeps its original start");
        assert!(still_held.serves());

        // And it re-opens the moment the backend answers again.
        assert_eq!(
            next_access(still_held, &viewer_probe(Some("recruit")), later),
            WarFeedAccess::Granted
        );
    }

    #[test]
    fn a_blackout_past_the_grace_window_stops_serving_but_keeps_asking() {
        let now = Instant::now();
        let held = WarFeedAccess::Unknown {
            serving: true,
            since: now,
        };
        let expired = next_access(
            held,
            &ViewerProbe::Unavailable,
            now + WAR_FEED_UNKNOWN_GRACE,
        );
        assert!(
            !expired.serves(),
            "a revocation must not outlive a long outage"
        );
        // But settling on `Denied` would mute the stream for good over an outage that never
        // said anything about this viewer.
        assert!(!expired.is_final());
        assert_eq!(
            next_access(
                expired,
                &viewer_probe(Some("chief")),
                now + WAR_FEED_UNKNOWN_GRACE
            ),
            WarFeedAccess::Granted,
            "the feed comes back when the backend does"
        );
    }

    #[test]
    fn a_stream_that_connected_during_an_outage_serves_nothing_but_can_open_later() {
        let now = Instant::now();
        let fresh = next_access(
            WarFeedAccess::Unknown {
                serving: false,
                since: now,
            },
            &ViewerProbe::Unavailable,
            now,
        );
        assert!(
            !fresh.serves(),
            "nothing goes out before access is confirmed"
        );
        assert!(!fresh.is_final(), "but it must not be written off either");
        assert_eq!(
            next_access(fresh, &viewer_probe(Some("chief")), now),
            WarFeedAccess::Granted
        );
    }

    #[test]
    fn a_denied_stream_is_never_probed_again() {
        let now = Instant::now();
        assert!(WarFeedAccess::Denied.is_final());
        // Even an unanswerable probe cannot move it, so an anonymous stream costs the
        // backend exactly one round trip for its whole life.
        assert_eq!(
            next_access(WarFeedAccess::Denied, &ViewerProbe::Unavailable, now),
            WarFeedAccess::Denied
        );
    }
}
