use std::cell::{Cell, RefCell};

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{EventSource, MessageEvent};

use sequoia_shared::TerritoryEvent;

use crate::app::{
    BufferedUpdate, CurrentMode, HistoryBufferModeActive, HistoryBufferSizeMax,
    HistoryBufferedUpdates, LastLiveSeq, LiveResyncInFlight, MapMode, NeedsLiveResync,
    SseSeqGapDetectedCount,
};
use crate::history;
use crate::territory::{ClientTerritoryMap, apply_changes, apply_runtime_updates, from_snapshot};

const LIVE_RESYNC_RETRY_BASE_MS: f64 = 500.0;
const LIVE_RESYNC_RETRY_MAX_MS: f64 = 10_000.0;
const RECONNECT_EVENT_TIMEOUT_MS: i32 = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Live,
    Reconnecting,
}

struct SseConnection {
    es: EventSource,
    on_open: Closure<dyn Fn()>,
    on_error: Closure<dyn Fn()>,
    snapshot_handler: Closure<dyn Fn(MessageEvent)>,
    update_handler: Closure<dyn Fn(MessageEvent)>,
    runtime_update_handler: Closure<dyn Fn(MessageEvent)>,
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

fn arm_reconnect_watchdog(
    mode: RwSignal<MapMode>,
    resync_in_flight: RwSignal<bool>,
    needs_live_resync: RwSignal<bool>,
    last_live_seq: RwSignal<Option<u64>>,
    territories: RwSignal<ClientTerritoryMap>,
) {
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
        needs_live_resync.set(true);
        trigger_live_resync(
            mode,
            resync_in_flight,
            needs_live_resync,
            last_live_seq,
            territories,
        );
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

fn trigger_live_resync(
    mode: RwSignal<MapMode>,
    resync_in_flight: RwSignal<bool>,
    needs_live_resync: RwSignal<bool>,
    last_live_seq: RwSignal<Option<u64>>,
    territories: RwSignal<ClientTerritoryMap>,
) {
    if mode.get_untracked() != MapMode::Live || resync_in_flight.get_untracked() {
        return;
    }

    let now_ms = js_sys::Date::now();
    if !live_resync_retry_ready(now_ms) {
        return;
    }

    resync_in_flight.set(true);
    spawn_local(async move {
        let result = history::fetch_live_state().await;
        resync_in_flight.set(false);

        if mode.get_untracked() != MapMode::Live {
            return;
        }

        match result {
            Ok(live_state) => {
                territories.set(from_snapshot(live_state.territories));
                last_live_seq.set(Some(live_state.seq));
                needs_live_resync.set(false);
                reset_live_resync_retry();
            }
            Err(e) => {
                needs_live_resync.set(true);
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
pub fn connect(territories: RwSignal<ClientTerritoryMap>, connection: RwSignal<ConnectionStatus>) {
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

    let CurrentMode(mode) = leptos::prelude::expect_context::<CurrentMode>();
    let HistoryBufferedUpdates(history_buffered_updates) =
        leptos::prelude::expect_context::<HistoryBufferedUpdates>();
    let HistoryBufferModeActive(buffer_mode_active) =
        leptos::prelude::expect_context::<HistoryBufferModeActive>();
    let HistoryBufferSizeMax(history_buffer_size_max) =
        leptos::prelude::expect_context::<HistoryBufferSizeMax>();
    let LastLiveSeq(last_live_seq) = leptos::prelude::expect_context::<LastLiveSeq>();
    let NeedsLiveResync(needs_live_resync) = leptos::prelude::expect_context::<NeedsLiveResync>();
    let LiveResyncInFlight(resync_in_flight) =
        leptos::prelude::expect_context::<LiveResyncInFlight>();
    let SseSeqGapDetectedCount(sse_seq_gap_detected_count) =
        leptos::prelude::expect_context::<SseSeqGapDetectedCount>();

    // On open
    let conn = connection;
    let on_open = Closure::<dyn Fn()>::new(move || {
        let was_reconnecting = conn.get_untracked() == ConnectionStatus::Reconnecting;
        if conn.get_untracked() != ConnectionStatus::Live {
            conn.set(ConnectionStatus::Live);
        }
        if was_reconnecting && mode.get_untracked() == MapMode::Live {
            arm_reconnect_watchdog(
                mode,
                resync_in_flight,
                needs_live_resync,
                last_live_seq,
                territories,
            );
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

        if mode.get_untracked() == MapMode::History || buffer_mode_active.get_untracked() {
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
                &format!(
                    "stale_sse_snapshot_ignored (last_seq={}, snapshot_seq={})",
                    last_seq, seq
                )
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

        if mode.get_untracked() == MapMode::History || buffer_mode_active.get_untracked() {
            if seq > 0 {
                history::buffer_history_update(
                    history_buffered_updates,
                    history_buffer_size_max,
                    needs_live_resync,
                    BufferedUpdate { seq, changes },
                );
            } else {
                needs_live_resync.set(true);
            }
            return;
        }

        if needs_live_resync.get_untracked() {
            trigger_live_resync(
                mode,
                resync_in_flight,
                needs_live_resync,
                last_live_seq,
                terr,
            );
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

            if history::has_seq_gap(Some(last_seq), seq) {
                let mut gap_count = 0;
                sse_seq_gap_detected_count.update(|count| {
                    *count = count.saturating_add(1);
                    gap_count = *count;
                });
                web_sys::console::warn_1(
                    &format!(
                        "sse_seq_gap_detected_count={} (last_seq={}, incoming_seq={})",
                        gap_count, last_seq, seq
                    )
                    .into(),
                );
                needs_live_resync.set(true);
                trigger_live_resync(
                    mode,
                    resync_in_flight,
                    needs_live_resync,
                    last_live_seq,
                    terr,
                );
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

        if mode.get_untracked() == MapMode::History || buffer_mode_active.get_untracked() {
            if seq > 0 {
                needs_live_resync.set(true);
            }
            return;
        }

        if needs_live_resync.get_untracked() {
            trigger_live_resync(
                mode,
                resync_in_flight,
                needs_live_resync,
                last_live_seq,
                terr,
            );
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

            if history::has_seq_gap(Some(last_seq), seq) {
                let mut gap_count = 0;
                sse_seq_gap_detected_count.update(|count| {
                    *count = count.saturating_add(1);
                    gap_count = *count;
                });
                web_sys::console::warn_1(
                    &format!(
                        "sse_seq_gap_detected_count={} (last_seq={}, incoming_seq={})",
                        gap_count, last_seq, seq
                    )
                    .into(),
                );
                needs_live_resync.set(true);
                trigger_live_resync(
                    mode,
                    resync_in_flight,
                    needs_live_resync,
                    last_live_seq,
                    terr,
                );
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
        });
    });
}
