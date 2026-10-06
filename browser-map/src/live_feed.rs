//! The live territory feed shared by both browser clients: the REST snapshot and the
//! `/api/events` SSE stream that keeps a territory map current.

use std::cell::{Cell, RefCell};

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{EventSource, MessageEvent};

use sequoia_map_engine::territory::{
    BufferedUpdate, ClientTerritoryMap, apply_changes, apply_runtime_updates, from_snapshot,
};
use sequoia_shared::{LiveState, TerritoryEvent, WarControllerState};

const MAX_BUFFERED_UPDATES: usize = 20_000;
const LIVE_RESYNC_RETRY_BASE_MS: f64 = 500.0;
const LIVE_RESYNC_RETRY_MAX_MS: f64 = 10_000.0;
const RECONNECT_EVENT_TIMEOUT_MS: i32 = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Live,
    Reconnecting,
}

/// The signals the feed keeps current.
#[derive(Clone, Copy)]
pub struct LiveFeed {
    pub territories: RwSignal<ClientTerritoryMap>,
    pub connection: RwSignal<ConnectionStatus>,
    /// While set, the map shows the past: updates are buffered for replay, not applied.
    pub history_mode: Signal<bool>,
    pub last_live_seq: RwSignal<Option<u64>>,
    pub buffered_updates: RwSignal<Vec<BufferedUpdate>>,
    /// Buffer updates even outside history mode (during the hand-off back to live).
    pub buffer_mode_active: RwSignal<bool>,
    pub buffer_size_max: RwSignal<usize>,
    pub needs_resync: RwSignal<bool>,
    pub resync_in_flight: RwSignal<bool>,
    pub seq_gap_count: RwSignal<u64>,
    /// Receives the war controller frames that ride the same stream, if wanted.
    pub war_controller: Option<RwSignal<Option<WarControllerState>>>,
}

impl LiveFeed {
    pub fn new(territories: RwSignal<ClientTerritoryMap>, history_mode: Signal<bool>) -> Self {
        Self {
            territories,
            connection: RwSignal::new(ConnectionStatus::Connecting),
            history_mode,
            last_live_seq: RwSignal::new(None),
            buffered_updates: RwSignal::new(Vec::new()),
            buffer_mode_active: RwSignal::new(false),
            buffer_size_max: RwSignal::new(0),
            needs_resync: RwSignal::new(false),
            resync_in_flight: RwSignal::new(false),
            seq_gap_count: RwSignal::new(0),
            war_controller: None,
        }
    }

    fn buffering(&self) -> bool {
        self.history_mode.get_untracked() || self.buffer_mode_active.get_untracked()
    }
}

/// Fetch a gap-free live snapshot with sequence.
pub async fn fetch_live_state() -> Result<LiveState, String> {
    let resp = gloo_net::http::Request::get("/api/live/state")
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<LiveState>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

fn has_seq_gap(last_live_seq: Option<u64>, incoming_seq: u64) -> bool {
    if incoming_seq == 0 {
        return false;
    }

    match last_live_seq {
        Some(last_seq) => incoming_seq != last_seq.saturating_add(1),
        None => false,
    }
}

/// Buffer one incoming live update while history mode is active.
fn buffer_history_update(feed: LiveFeed, update: BufferedUpdate) {
    let mut overflowed = false;
    let mut new_len = 0;

    feed.buffered_updates.update(|buffer| {
        if buffer.iter().any(|existing| existing.seq == update.seq) {
            new_len = buffer.len();
            return;
        }

        buffer.push(update);
        buffer.sort_by_key(|item| item.seq);

        if buffer.len() > MAX_BUFFERED_UPDATES {
            let overflow = buffer.len() - MAX_BUFFERED_UPDATES;
            buffer.drain(0..overflow);
            overflowed = true;
        }

        new_len = buffer.len();
    });

    if overflowed {
        feed.needs_resync.set(true);
        web_sys::console::warn_1(
            &"history buffer overflowed; forcing live resync on handoff".into(),
        );
    }

    let mut updated_max = None;
    feed.buffer_size_max.update(|current_max| {
        if new_len > *current_max {
            *current_max = new_len;
            updated_max = Some(new_len);
        }
    });
    if let Some(max_size) = updated_max {
        web_sys::console::info_1(&format!("history_buffer_size_max={max_size}").into());
    }
}

struct SseConnection {
    es: EventSource,
    on_open: Closure<dyn Fn()>,
    on_error: Closure<dyn Fn()>,
    snapshot_handler: Closure<dyn Fn(MessageEvent)>,
    update_handler: Closure<dyn Fn(MessageEvent)>,
    runtime_update_handler: Closure<dyn Fn(MessageEvent)>,
    warcontroller_handler: Closure<dyn Fn(MessageEvent)>,
}

struct ReconnectWatchdog {
    window: web_sys::Window,
    timeout_id: i32,
    _callback: Closure<dyn Fn()>,
}

impl SseConnection {
    fn close(self) {
        let _ = self.on_open.as_ref();
        let _ = self.on_error.as_ref();
        self.es.set_onopen(None);
        self.es.set_onerror(None);
        self.es
            .remove_event_listener_with_callback(
                "snapshot",
                self.snapshot_handler.as_ref().unchecked_ref(),
            )
            .ok();
        self.es
            .remove_event_listener_with_callback(
                "update",
                self.update_handler.as_ref().unchecked_ref(),
            )
            .ok();
        self.es
            .remove_event_listener_with_callback(
                "runtime_update",
                self.runtime_update_handler.as_ref().unchecked_ref(),
            )
            .ok();
        self.es
            .remove_event_listener_with_callback(
                "warcontroller",
                self.warcontroller_handler.as_ref().unchecked_ref(),
            )
            .ok();
        self.es.close();
    }
}

#[derive(Debug, Clone, Copy)]
struct LiveResyncRetryState {
    consecutive_failures: u32,
    next_allowed_at_ms: f64,
}

impl LiveResyncRetryState {
    const fn new() -> Self {
        Self {
            consecutive_failures: 0,
            next_allowed_at_ms: 0.0,
        }
    }
}

thread_local! {
    static SSE_CONNECTION: RefCell<Option<SseConnection>> = const { RefCell::new(None) };
    static LIVE_RESYNC_RETRY: RefCell<LiveResyncRetryState> = const { RefCell::new(LiveResyncRetryState::new()) };
    static POST_RECONNECT_AWAITING_EVENT: Cell<bool> = const { Cell::new(false) };
    static RECONNECT_WATCHDOG: RefCell<Option<ReconnectWatchdog>> = const { RefCell::new(None) };
}

pub fn disconnect() {
    SSE_CONNECTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(connection) = slot.take() {
            connection.close();
        }
    });
    clear_reconnect_watchdog();
    POST_RECONNECT_AWAITING_EVENT.with(|awaiting| awaiting.set(false));
    reset_live_resync_retry();
}

fn live_resync_backoff_ms(consecutive_failures: u32) -> f64 {
    let exponent = consecutive_failures.saturating_sub(1).min(6);
    let factor = 1u32 << exponent;
    (LIVE_RESYNC_RETRY_BASE_MS * factor as f64).min(LIVE_RESYNC_RETRY_MAX_MS)
}

fn reset_live_resync_retry() {
    LIVE_RESYNC_RETRY.with(|state| {
        *state.borrow_mut() = LiveResyncRetryState::new();
    });
}

fn live_resync_retry_ready(now_ms: f64) -> bool {
    LIVE_RESYNC_RETRY.with(|state| now_ms >= state.borrow().next_allowed_at_ms)
}

fn mark_live_resync_failure(now_ms: f64) -> (u32, f64) {
    LIVE_RESYNC_RETRY.with(|state| {
        let mut state = state.borrow_mut();
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let backoff_ms = live_resync_backoff_ms(state.consecutive_failures);
        state.next_allowed_at_ms = now_ms + backoff_ms;
        (state.consecutive_failures, backoff_ms)
    })
}

fn clear_reconnect_watchdog() {
    RECONNECT_WATCHDOG.with(|slot| {
        if let Some(binding) = slot.borrow_mut().take() {
            binding.window.clear_timeout_with_handle(binding.timeout_id);
        }
    });
}

fn arm_reconnect_watchdog(feed: LiveFeed) {
    clear_reconnect_watchdog();
    POST_RECONNECT_AWAITING_EVENT.with(|awaiting| awaiting.set(true));

    let Some(window) = web_sys::window() else {
        return;
    };

    let callback = Closure::<dyn Fn()>::new(move || {
        let should_trigger = POST_RECONNECT_AWAITING_EVENT.with(|awaiting| {
            if !awaiting.get() {
                return false;
            }
            awaiting.set(false);
            true
        });
        if !should_trigger {
            return;
        }
        feed.needs_resync.set(true);
        trigger_live_resync(feed);
    });

    let Ok(timeout_id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        RECONNECT_EVENT_TIMEOUT_MS,
    ) else {
        return;
    };

    RECONNECT_WATCHDOG.with(|slot| {
        *slot.borrow_mut() = Some(ReconnectWatchdog {
            window: window.clone(),
            timeout_id,
            _callback: callback,
        });
    });
}

fn mark_post_reconnect_event_received() {
    let was_waiting = POST_RECONNECT_AWAITING_EVENT.with(|awaiting| {
        let was_waiting = awaiting.get();
        if was_waiting {
            awaiting.set(false);
        }
        was_waiting
    });
    if was_waiting {
        clear_reconnect_watchdog();
    }
}

fn trigger_live_resync(feed: LiveFeed) {
    if feed.history_mode.get_untracked() || feed.resync_in_flight.get_untracked() {
        return;
    }

    let now_ms = js_sys::Date::now();
    if !live_resync_retry_ready(now_ms) {
        return;
    }

    feed.resync_in_flight.set(true);
    spawn_local(async move {
        let result = fetch_live_state().await;
        feed.resync_in_flight.set(false);

        if feed.history_mode.get_untracked() {
            return;
        }

        match result {
            Ok(live_state) => {
                feed.territories.set(from_snapshot(live_state.territories));
                feed.last_live_seq.set(Some(live_state.seq));
                feed.needs_resync.set(false);
                reset_live_resync_retry();
            }
            Err(e) => {
                feed.needs_resync.set(true);
                let (attempt, backoff_ms) = mark_live_resync_failure(js_sys::Date::now());
                web_sys::console::warn_1(
                    &format!(
                        "Live resync failed (attempt {attempt}): {e}; backing off for {}ms",
                        backoff_ms.round()
                    )
                    .into(),
                );
            }
        }
    });
}

/// Connect to the SSE endpoint and reactively update territory state.
pub fn connect(feed: LiveFeed) {
    let LiveFeed {
        territories,
        connection,
        last_live_seq,
        needs_resync: needs_live_resync,
        seq_gap_count: sse_seq_gap_detected_count,
        ..
    } = feed;
    clear_reconnect_watchdog();
    POST_RECONNECT_AWAITING_EVENT.with(|awaiting| awaiting.set(false));
    connection.set(ConnectionStatus::Connecting);

    let es = match EventSource::new("/api/events") {
        Ok(es) => es,
        Err(_) => {
            connection.set(ConnectionStatus::Reconnecting);
            return;
        }
    };

    // On open
    let conn = connection;
    let on_open = Closure::<dyn Fn()>::new(move || {
        let was_reconnecting = conn.get_untracked() == ConnectionStatus::Reconnecting;
        if conn.get_untracked() != ConnectionStatus::Live {
            conn.set(ConnectionStatus::Live);
        }
        if was_reconnecting && !feed.history_mode.get_untracked() {
            arm_reconnect_watchdog(feed);
        }
    });
    es.set_onopen(Some(on_open.as_ref().unchecked_ref()));

    // On "snapshot" event
    let terr = territories;
    let snapshot_handler = Closure::<dyn Fn(MessageEvent)>::new(move |e: MessageEvent| {
        let Some(data) = e.data().as_string() else {
            return;
        };

        let Ok(TerritoryEvent::Snapshot {
            seq,
            territories: map,
            ..
        }) = serde_json::from_str::<TerritoryEvent>(&data)
        else {
            return;
        };
        mark_post_reconnect_event_received();

        if feed.buffering() {
            if seq > 0 {
                needs_live_resync.set(true);
            }
            return;
        }

        if let Some(last_seq) = last_live_seq.get_untracked()
            && seq > 0
            && seq < last_seq
        {
            web_sys::console::info_1(
                &format!("stale_sse_snapshot_ignored (last_seq={last_seq}, snapshot_seq={seq})")
                    .into(),
            );
            return;
        }

        terr.set(from_snapshot(map));
        if seq > 0 {
            last_live_seq.set(Some(seq));
        } else {
            last_live_seq.set(None);
        }
        needs_live_resync.set(false);
        reset_live_resync_retry();
    });
    es.add_event_listener_with_callback("snapshot", snapshot_handler.as_ref().unchecked_ref())
        .ok();

    // On "update" event
    let terr = territories;
    let update_handler = Closure::<dyn Fn(MessageEvent)>::new(move |e: MessageEvent| {
        let Some(data) = e.data().as_string() else {
            return;
        };

        let Ok(TerritoryEvent::Update { seq, changes, .. }) =
            serde_json::from_str::<TerritoryEvent>(&data)
        else {
            return;
        };
        mark_post_reconnect_event_received();

        if feed.buffering() {
            if seq > 0 {
                buffer_history_update(feed, BufferedUpdate { seq, changes });
            } else {
                needs_live_resync.set(true);
            }
            return;
        }

        if needs_live_resync.get_untracked() {
            trigger_live_resync(feed);
            return;
        }

        if seq == 0 {
            // Legacy event payload without sequence IDs.
            let now = js_sys::Date::now();
            terr.update(|map| {
                apply_changes(map, &changes, now, 800.0);
            });
            last_live_seq.set(None);
            return;
        }

        if let Some(last_seq) = last_live_seq.get_untracked() {
            if seq <= last_seq {
                return;
            }

            if has_seq_gap(Some(last_seq), seq) {
                let mut gap_count = 0;
                sse_seq_gap_detected_count.update(|count| {
                    *count = count.saturating_add(1);
                    gap_count = *count;
                });
                web_sys::console::warn_1(
                    &format!(
                        "sse_seq_gap_detected_count={gap_count} (last_seq={last_seq}, incoming_seq={seq})"
                    )
                    .into(),
                );
                needs_live_resync.set(true);
                trigger_live_resync(feed);
                return;
            }
        }

        let now = js_sys::Date::now();
        terr.update(|map| {
            apply_changes(map, &changes, now, 800.0);
        });
        last_live_seq.set(Some(seq));
    });
    es.add_event_listener_with_callback("update", update_handler.as_ref().unchecked_ref())
        .ok();

    // On "runtime_update" event
    let terr = territories;
    let runtime_update_handler = Closure::<dyn Fn(MessageEvent)>::new(move |e: MessageEvent| {
        let Some(data) = e.data().as_string() else {
            return;
        };

        let Ok(TerritoryEvent::RuntimeUpdate { seq, updates, .. }) =
            serde_json::from_str::<TerritoryEvent>(&data)
        else {
            return;
        };
        mark_post_reconnect_event_received();

        if feed.buffering() {
            if seq > 0 {
                needs_live_resync.set(true);
            }
            return;
        }

        if needs_live_resync.get_untracked() {
            trigger_live_resync(feed);
            return;
        }

        if seq == 0 {
            terr.update(|map| {
                apply_runtime_updates(map, &updates);
            });
            last_live_seq.set(None);
            return;
        }

        if let Some(last_seq) = last_live_seq.get_untracked() {
            if seq <= last_seq {
                return;
            }

            if has_seq_gap(Some(last_seq), seq) {
                let mut gap_count = 0;
                sse_seq_gap_detected_count.update(|count| {
                    *count = count.saturating_add(1);
                    gap_count = *count;
                });
                web_sys::console::warn_1(
                    &format!(
                        "sse_seq_gap_detected_count={gap_count} (last_seq={last_seq}, incoming_seq={seq})"
                    )
                    .into(),
                );
                needs_live_resync.set(true);
                trigger_live_resync(feed);
                return;
            }
        }

        terr.update(|map| {
            apply_runtime_updates(map, &updates);
        });
        last_live_seq.set(Some(seq));
    });
    es.add_event_listener_with_callback(
        "runtime_update",
        runtime_update_handler.as_ref().unchecked_ref(),
    )
    .ok();

    // War controller state rides this connection but is outside the territory sequence
    // stream, so it must not touch last_live_seq or needs_live_resync.
    let warcontroller_handler = Closure::<dyn Fn(MessageEvent)>::new(move |e: MessageEvent| {
        let Some(data) = e.data().as_string() else {
            return;
        };
        match serde_json::from_str::<WarControllerState>(&data) {
            // Seeds, lag replays and live frames all arrive here and can interleave, so an
            // older payload must never overwrite a newer one.
            // `maybe_update` so a dropped frame does not notify: the feed re-broadcasts often
            // enough that a needless wake would ripple through every war memo.
            Ok(state) => {
                if let Some(war_controller) = feed.war_controller {
                    war_controller.maybe_update(|current| {
                        if !state.supersedes(current.as_ref()) {
                            return false;
                        }
                        *current = Some(state);
                        true
                    });
                }
            }
            Err(error) => {
                web_sys::console::warn_1(
                    &format!("war controller event decode failed: {error}").into(),
                );
            }
        }
    });
    es.add_event_listener_with_callback(
        "warcontroller",
        warcontroller_handler.as_ref().unchecked_ref(),
    )
    .ok();

    // On error
    let conn = connection;
    let on_error = Closure::<dyn Fn()>::new(move || {
        if conn.get_untracked() != ConnectionStatus::Reconnecting {
            conn.set(ConnectionStatus::Reconnecting);
        }
    });
    es.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    // Replace any existing connection, ensuring handlers are unregistered cleanly.
    SSE_CONNECTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(old) = slot.take() {
            old.close();
        }
        *slot = Some(SseConnection {
            es,
            on_open,
            on_error,
            snapshot_handler,
            update_handler,
            runtime_update_handler,
            warcontroller_handler,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::has_seq_gap;

    #[test]
    fn detects_sequence_gap() {
        assert!(!has_seq_gap(Some(10), 11));
        assert!(has_seq_gap(Some(10), 12));
        assert!(!has_seq_gap(None, 7));
        assert!(!has_seq_gap(Some(10), 0));
    }
}
