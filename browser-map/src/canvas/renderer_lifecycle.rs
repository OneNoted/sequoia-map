//! Starting the renderer and keeping it running: the first start, failed starts, a lost and
//! restored WebGL context, manual retries and unmounting all go through this one owner.
//!
//! A start the browser refuses is retried a few times on a growing delay. After that the
//! map shows the failure with a Retry button, which works at most once per cooldown. A
//! context the browser takes away is asked back with `preventDefault` and rebuilt once it
//! returns. Every start carries an attempt number, and one that finishes after a loss, a
//! newer start or unmount superseded it is dropped, so only the current start can install
//! a renderer. A superseded start also stops: one that has not begun never touches the
//! canvas, and one waiting on the browser finishes that step and goes no further.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};
use std::time::Duration;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::HtmlCanvasElement;

use super::MapState;
use crate::frame::{InitError, InitStage};
use crate::gpu::GpuRenderer;
use sequoia_map_engine::scene::ScenePlanner;

/// How long to wait before each automatic restart after a failed start.
const RESTART_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(4),
];
/// The least time between two manual retries.
const RETRY_COOLDOWN_MS: f64 = 2_000.0;

/// One renderer start. Only the latest can install its renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Attempt(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Unstarted,
    /// The current attempt is running.
    Starting,
    /// An automatic restart is due.
    Waiting,
    Ready,
    /// The browser took the context; it is rebuilt once restored.
    Lost,
    /// Automatic restarts are spent; only Retry starts again.
    Failed,
    Disposed,
}

/// What to do once a start finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finished {
    /// Superseded: drop whatever it made.
    Stale,
    Ready,
    RestartAfter(Duration),
    Failed,
}

/// Whether the failure panel's Retry button can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryState {
    Available,
    Running,
    CoolingDown(Duration),
}

/// The lifecycle's decisions, apart from the browser so they can be tested on the host.
#[derive(Debug)]
struct Lifecycle {
    phase: Phase,
    attempt: Attempt,
    /// Starts since mount, for diagnostics.
    starts: u32,
    /// Failed starts since the renderer last ran or the context last came back.
    failures: usize,
    /// The current start is a manual retry, which gets no automatic restarts.
    manual: bool,
    /// When Retry next works, on the `performance.now()` clock.
    retry_at_ms: f64,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            phase: Phase::Unstarted,
            attempt: Attempt::default(),
            starts: 0,
            failures: 0,
            manual: false,
            retry_at_ms: f64::NEG_INFINITY,
        }
    }

    fn mount(&mut self) -> Option<Attempt> {
        (self.phase == Phase::Unstarted).then(|| self.start(false))
    }

    fn is_current(&self, attempt: Attempt) -> bool {
        self.phase == Phase::Starting && attempt == self.attempt
    }

    fn finished(&mut self, attempt: Attempt, ran: bool) -> Finished {
        if !self.is_current(attempt) {
            return Finished::Stale;
        }
        if ran {
            self.phase = Phase::Ready;
            self.failures = 0;
            return Finished::Ready;
        }
        self.failures += 1;
        let restart = (!self.manual)
            .then(|| RESTART_DELAYS.get(self.failures - 1))
            .flatten();
        match restart {
            Some(&delay) => {
                self.phase = Phase::Waiting;
                Finished::RestartAfter(delay)
            }
            None => {
                self.phase = Phase::Failed;
                Finished::Failed
            }
        }
    }

    /// The restart scheduled while waiting after `failed`, unless something superseded it.
    fn restart_due(&mut self, failed: Attempt) -> Option<Attempt> {
        (self.phase == Phase::Waiting && failed == self.attempt).then(|| self.start(false))
    }

    fn retry(&mut self, now_ms: f64) -> Option<Attempt> {
        if self.phase != Phase::Failed || now_ms < self.retry_at_ms {
            return None;
        }
        self.retry_at_ms = now_ms + RETRY_COOLDOWN_MS;
        Some(self.start(true))
    }

    fn retry_state(&self, now_ms: f64) -> RetryState {
        if self.phase == Phase::Starting {
            RetryState::Running
        } else if now_ms < self.retry_at_ms {
            RetryState::CoolingDown(Duration::from_secs_f64(
                (self.retry_at_ms - now_ms) / 1_000.0,
            ))
        } else {
            RetryState::Available
        }
    }

    /// Returns whether the renderer must go.
    fn lose(&mut self) -> bool {
        if matches!(self.phase, Phase::Unstarted | Phase::Disposed) {
            return false;
        }
        self.supersede(Phase::Lost);
        self.failures = 0;
        true
    }

    fn restore(&mut self) -> Option<Attempt> {
        (self.phase == Phase::Lost).then(|| self.start(false))
    }

    fn dispose(&mut self) {
        self.supersede(Phase::Disposed);
    }

    fn start(&mut self, manual: bool) -> Attempt {
        self.supersede(Phase::Starting);
        self.starts += 1;
        self.manual = manual;
        self.attempt
    }

    fn supersede(&mut self, phase: Phase) {
        self.attempt = Attempt(self.attempt.0.wrapping_add(1));
        self.phase = phase;
    }
}

/// What a failed start looked like, for the console and the failure panel's details.
#[derive(Clone, Debug, PartialEq)]
struct StartFailure {
    error: InitError,
    /// `webglcontextcreationerror`'s status message, when the browser gave one.
    browser_message: Option<String>,
    start: u32,
    canvas: (u32, u32),
    dpr: f64,
    hidden: bool,
    /// The hashed name of the client's WebAssembly module, which names its build.
    client: Option<String>,
}

impl fmt::Display for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (width, height) = self.canvas;
        write!(
            f,
            "stage={} start={} canvas={width}x{height} dpr={} visibility={} client={}",
            self.error.stage.name(),
            self.start,
            self.dpr,
            if self.hidden { "hidden" } else { "visible" },
            self.client.as_deref().unwrap_or("unknown"),
        )?;
        if let Some(message) = &self.browser_message {
            write!(f, " browser={message:?}")?;
        }
        write!(f, " detail={:?}", self.error.detail)
    }
}

/// The failure panel's content: the last failure and whether Retry works now.
#[derive(Clone, Debug, PartialEq)]
struct FailureView {
    failure: StartFailure,
    retry: RetryState,
}

/// A pending `setTimeout`, cleared when dropped.
struct Timeout {
    handle: i32,
    _callback: Closure<dyn FnMut()>,
}

impl Timeout {
    fn new(delay: Duration, callback: impl FnMut() + 'static) -> Option<Self> {
        let callback = Closure::<dyn FnMut()>::new(callback);
        let handle = web_sys::window()?
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                // Rounded up: an early wake would find the cooldown not quite over.
                (delay.as_secs_f64() * 1_000.0)
                    .ceil()
                    .min(f64::from(i32::MAX)) as i32,
            )
            .ok()?;
        Some(Self {
            handle,
            _callback: callback,
        })
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.handle);
        }
    }
}

/// The canvas's renderer owner. The canvas forwards its mount, context events and unmount.
pub(super) struct RendererLifecycle {
    lifecycle: RefCell<Lifecycle>,
    state: Rc<RefCell<MapState>>,
    canvas: NodeRef<leptos::html::Canvas>,
    /// The one pending restart or Retry cooldown.
    timer: RefCell<Option<Timeout>>,
    /// The browser's reason for the last refused context, until the start it belongs to ends.
    browser_message: RefCell<Option<String>>,
    /// Size, DPR and visibility when the current attempt began.
    started_with: Cell<((u32, u32), f64, bool)>,
    installed: Trigger,
    failure: RwSignal<Option<FailureView>>,
}

impl RendererLifecycle {
    pub(super) fn new(
        state: Rc<RefCell<MapState>>,
        canvas: NodeRef<leptos::html::Canvas>,
    ) -> Rc<Self> {
        Rc::new(Self {
            lifecycle: RefCell::new(Lifecycle::new()),
            state,
            canvas,
            timer: RefCell::new(None),
            browser_message: RefCell::new(None),
            started_with: Cell::new(((0, 0), 1.0, false)),
            installed: Trigger::new(),
            failure: RwSignal::new(None),
        })
    }

    /// Subscribes the caller to renderer installs.
    pub(super) fn track(&self) {
        self.installed.track();
    }

    /// Changes whenever a renderer is superseded, so work begun for one can tell.
    pub(super) fn attempt(&self) -> Attempt {
        self.lifecycle.borrow().attempt
    }

    pub(super) fn is_disposed(&self) -> bool {
        self.lifecycle.borrow().phase == Phase::Disposed
    }

    pub(super) fn is_lost(&self) -> bool {
        self.lifecycle.borrow().phase == Phase::Lost
    }

    /// Starts the renderer the first time the canvas is in the document.
    pub(super) fn mounted(self: &Rc<Self>) {
        let start = self.lifecycle.borrow_mut().mount();
        if let Some(attempt) = start {
            self.run(attempt);
        }
    }

    pub(super) fn context_lost(&self, event: &web_sys::Event) {
        // Without this the browser never gives the context back.
        event.prevent_default();
        if !self.lifecycle.borrow_mut().lose() {
            return;
        }
        web_sys::console::warn_1(&"map: WebGL context lost; waiting for it to be restored".into());
        self.cancel();
        self.failure.set(None);
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.renderer = None;
        }
    }

    pub(super) fn context_restored(self: &Rc<Self>) {
        let start = self.lifecycle.borrow_mut().restore();
        if let Some(attempt) = start {
            web_sys::console::warn_1(
                &"map: WebGL context restored; rebuilding the renderer".into(),
            );
            self.run(attempt);
        }
    }

    fn restart(self: &Rc<Self>, waiting: Attempt) {
        let start = self.lifecycle.borrow_mut().restart_due(waiting);
        if let Some(attempt) = start {
            self.run(attempt);
        }
    }

    /// Keeps the browser's reason for refusing a context; it fires inside `getContext`.
    pub(super) fn context_creation_error(&self, event: &web_sys::Event) {
        let message = js_sys::Reflect::get(event, &"statusMessage".into())
            .ok()
            .and_then(|message| message.as_string())
            .filter(|message| !message.is_empty());
        *self.browser_message.borrow_mut() = message;
    }

    fn retry(self: &Rc<Self>) {
        let start = self.lifecycle.borrow_mut().retry(now_ms());
        if let Some(attempt) = start {
            self.publish_failure_state();
            self.run(attempt);
        }
    }

    pub(super) fn dispose(&self) {
        self.lifecycle.borrow_mut().dispose();
        self.cancel();
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.renderer = None;
        }
    }

    fn run(self: &Rc<Self>, attempt: Attempt) {
        let Some(canvas) = self.canvas.get_untracked() else {
            return;
        };
        self.browser_message.borrow_mut().take();
        self.started_with.set(start_context(&canvas));
        // Unmounting takes the canvas out of the document before its cleanup reaches the
        // lifecycle, so a start that is still current but whose canvas has gone is not wanted
        // either.
        let wanted = {
            let this = Rc::downgrade(self);
            let canvas = canvas.clone();
            move || {
                canvas.is_connected()
                    && this
                        .upgrade()
                        .is_some_and(|this| this.lifecycle.borrow().is_current(attempt))
            }
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if !wanted() {
                return;
            }
            let result = GpuRenderer::init(canvas, wanted).await;
            this.finished(attempt, result);
        });
    }

    /// Stops any pending restart or cooldown. A running start sees it is superseded at its
    /// next step.
    fn cancel(&self) {
        self.timer.borrow_mut().take();
    }

    fn finished(self: &Rc<Self>, attempt: Attempt, result: Result<GpuRenderer, InitError>) {
        let (finished, start, failures) = {
            let mut lifecycle = self.lifecycle.borrow_mut();
            let failures = lifecycle.failures;
            (
                lifecycle.finished(attempt, result.is_ok()),
                lifecycle.starts,
                failures,
            )
        };
        let error = match (finished, result) {
            (Finished::Stale, _) => return,
            (Finished::Ready, Ok(renderer)) => {
                if failures > 0 {
                    web_sys::console::info_1(
                        &format!("map renderer: started after {failures} failed starts").into(),
                    );
                }
                {
                    let mut state = self.state.borrow_mut();
                    state.renderer = Some(renderer);
                    // A new renderer starts with empty caches.
                    state.planner = ScenePlanner::default();
                }
                self.failure.set(None);
                self.installed.notify();
                return;
            }
            (_, Err(error)) => error,
            (_, Ok(_)) => unreachable!("a start that succeeded finishes ready or stale"),
        };
        let ((width, height), dpr, hidden) = self.started_with.get();
        let failure = StartFailure {
            error,
            browser_message: self.browser_message.borrow_mut().take(),
            start,
            canvas: (width, height),
            dpr,
            hidden,
            client: client_build(),
        };
        match finished {
            Finished::RestartAfter(delay) => {
                web_sys::console::warn_1(
                    &format!(
                        "map renderer: start failed, restarting in {} ms: {failure}",
                        delay.as_millis()
                    )
                    .into(),
                );
                self.schedule(delay, move |this| this.restart(attempt));
            }
            Finished::Failed => {
                web_sys::console::error_1(
                    &format!("map renderer: start failed, waiting for Retry: {failure}").into(),
                );
                self.failure.set(Some(FailureView {
                    failure,
                    retry: RetryState::Available,
                }));
                self.publish_failure_state();
            }
            Finished::Stale | Finished::Ready => {}
        }
    }

    /// Shows whether Retry works now, and re-enables it when its cooldown ends.
    fn publish_failure_state(self: &Rc<Self>) {
        let retry = self.lifecycle.borrow().retry_state(now_ms());
        self.failure.update(|view| {
            if let Some(view) = view {
                view.retry = retry;
            }
        });
        if let RetryState::CoolingDown(wait) = retry {
            self.schedule(wait, |this| this.publish_failure_state());
        }
    }

    fn schedule(self: &Rc<Self>, delay: Duration, run: impl Fn(&Rc<Self>) + 'static) {
        let this = Rc::downgrade(self);
        *self.timer.borrow_mut() = Timeout::new(delay, move || {
            if let Some(this) = Weak::upgrade(&this) {
                run(&this);
            }
        });
    }

    pub(super) fn view_failure(self: &Rc<Self>) -> impl IntoView + use<> {
        let failure = self.failure;
        let this = StoredValue::new_local(self.clone());
        move || {
            failure.get().map(|view| {
                failure_panel(view, move || {
                    if let Some(this) = this.try_get_value() {
                        this.retry();
                    }
                })
            })
        }
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

/// The canvas's backing size, the device pixel ratio and whether the page is hidden.
fn start_context(canvas: &HtmlCanvasElement) -> ((u32, u32), f64, bool) {
    let window = web_sys::window();
    let dpr = window
        .as_ref()
        .map_or(1.0, web_sys::Window::device_pixel_ratio);
    let hidden = window
        .and_then(|window| window.document())
        .is_some_and(|document| document.hidden());
    ((canvas.width(), canvas.height()), dpr, hidden)
}

/// The client build, from the WebAssembly module the page loaded.
fn client_build() -> Option<String> {
    let entries = web_sys::window()?
        .performance()?
        .get_entries_by_type("resource");
    entries.iter().find_map(|entry| {
        let url = js_sys::Reflect::get(&entry, &"name".into())
            .ok()?
            .as_string()?;
        let file = url.split(['?', '#']).next()?.rsplit('/').next()?;
        file.strip_suffix("_bg.wasm").map(str::to_owned)
    })
}

fn diagnostics_token(message: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in message.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("GPUX-{hash:016x}")
}

fn failure_panel(view: FailureView, retry: impl Fn() + 'static) -> impl IntoView {
    let FailureView {
        failure,
        retry: retry_state,
    } = view;
    let explanation = match failure.error.stage {
        InitStage::Surface => {
            "The browser didn't let the map use graphics. This is often temporary."
        }
        _ => "The map's graphics setup failed in this browser.",
    };
    let token = diagnostics_token(&format!(
        "{}: {}",
        failure.error.stage.name(),
        failure.error.detail
    ));
    let details = failure.to_string();
    let retry_label = if retry_state == RetryState::Running {
        "Retrying…"
    } else {
        "Retry"
    };
    let button = "font-family: var(--font-display); font-size: 0.72rem; letter-spacing: 0.06em; padding: 8px 14px; border-radius: 4px; cursor: pointer;";
    view! {
        <div
            role="alert"
            style="position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; background: rgba(12, 14, 23, 0.96); z-index: 30;"
        >
            <div style="max-width: 480px; margin: 0 24px; border: 1px solid var(--color-border-accent); background: var(--color-deep); box-shadow: 0 24px 64px rgba(0,0,0,0.55); border-radius: 8px; padding: 22px 20px;">
                <div style="font-family: var(--font-display); color: var(--color-gold); letter-spacing: 0.08em; font-size: 0.78rem; text-transform: uppercase; margin-bottom: 8px;">
                    "Map couldn't start"
                </div>
                <div style="font-family: var(--font-body); color: var(--color-text-primary); line-height: 1.45; font-size: 0.92rem;">
                    {explanation}
                </div>
                <div style="display: flex; gap: 10px; margin-top: 16px;">
                    <button
                        type="button"
                        disabled=retry_state != RetryState::Available
                        style=format!("{button} border: 1px solid var(--color-gold); background: rgba(245,197,66,0.14); color: var(--color-gold);")
                        on:click=move |_| retry()
                    >
                        {retry_label}
                    </button>
                    <button
                        type="button"
                        style=format!("{button} border: 1px solid var(--color-border-accent); background: transparent; color: var(--color-text-secondary);")
                        on:click=|_| {
                            if let Some(window) = web_sys::window() {
                                let _ = window.location().reload();
                            }
                        }
                    >
                        "Reload page"
                    </button>
                </div>
                <details style="margin-top: 14px; font-family: var(--font-mono); font-size: 0.7rem; color: var(--color-text-dim);">
                    <summary style="cursor: pointer;">"Details"</summary>
                    <div style="margin-top: 6px; color: var(--color-text-secondary); word-break: break-word;">
                        {details}
                    </div>
                    <div style="margin-top: 6px;">
                        "Diagnostics token: "
                        <span style="color: var(--color-gold);">{token}</span>
                    </div>
                </details>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(start: Option<Attempt>) -> Attempt {
        start.expect("a start")
    }

    fn fail(lifecycle: &mut Lifecycle, attempt: Attempt) -> Finished {
        lifecycle.finished(attempt, false)
    }

    fn run(lifecycle: &mut Lifecycle, attempt: Attempt) -> Finished {
        lifecycle.finished(attempt, true)
    }

    #[test]
    fn mounting_starts_once() {
        let mut lifecycle = Lifecycle::new();
        assert!(lifecycle.mount().is_some());
        assert_eq!(lifecycle.mount(), None);
    }

    #[test]
    fn refused_starts_restart_on_growing_delays_then_wait_for_retry() {
        let mut lifecycle = Lifecycle::new();
        let mut current = attempt(lifecycle.mount());
        for delay in RESTART_DELAYS {
            assert_eq!(fail(&mut lifecycle, current), Finished::RestartAfter(delay));
            current = attempt(lifecycle.restart_due(current));
        }
        assert_eq!(fail(&mut lifecycle, current), Finished::Failed);
        assert_eq!(lifecycle.restart_due(current), None);
        assert_eq!(lifecycle.starts, 1 + RESTART_DELAYS.len() as u32);
    }

    #[test]
    fn one_refusal_recovers_on_the_first_restart() {
        let mut lifecycle = Lifecycle::new();
        let first = attempt(lifecycle.mount());
        assert!(matches!(
            fail(&mut lifecycle, first),
            Finished::RestartAfter(_)
        ));
        let second = attempt(lifecycle.restart_due(first));
        assert_eq!(run(&mut lifecycle, second), Finished::Ready);
        assert_eq!(lifecycle.retry(f64::INFINITY), None);
    }

    #[test]
    fn retry_runs_one_start_and_then_cools_down() {
        let mut lifecycle = Lifecycle::new();
        let mut current = attempt(lifecycle.mount());
        while fail(&mut lifecycle, current) != Finished::Failed {
            current = attempt(lifecycle.restart_due(current));
        }
        assert_eq!(lifecycle.retry_state(0.0), RetryState::Available);

        let retried = attempt(lifecycle.retry(0.0));
        assert_eq!(lifecycle.retry_state(10.0), RetryState::Running);
        assert_eq!(lifecycle.retry(10.0), None);
        assert_eq!(fail(&mut lifecycle, retried), Finished::Failed);
        assert_eq!(
            lifecycle.retry_state(500.0),
            RetryState::CoolingDown(Duration::from_millis(1_500))
        );
        assert_eq!(lifecycle.retry(500.0), None);

        let retried = attempt(lifecycle.retry(RETRY_COOLDOWN_MS));
        assert_eq!(run(&mut lifecycle, retried), Finished::Ready);
    }

    #[test]
    fn retry_only_follows_a_failure() {
        let mut lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.retry(0.0), None);
        let first = attempt(lifecycle.mount());
        assert_eq!(lifecycle.retry(0.0), None);
        run(&mut lifecycle, first);
        assert_eq!(lifecycle.retry(0.0), None);
    }

    #[test]
    fn a_loss_supersedes_the_running_start_and_restoring_starts_again() {
        let mut lifecycle = Lifecycle::new();
        let first = attempt(lifecycle.mount());
        assert!(lifecycle.lose());
        assert_eq!(run(&mut lifecycle, first), Finished::Stale);
        let restored = attempt(lifecycle.restore());
        assert_ne!(restored, first);
        assert_eq!(run(&mut lifecycle, restored), Finished::Ready);
        assert_eq!(lifecycle.restore(), None);
    }

    #[test]
    fn a_loss_cancels_a_pending_restart_and_resets_the_budget() {
        let mut lifecycle = Lifecycle::new();
        let mut current = attempt(lifecycle.mount());
        fail(&mut lifecycle, current);
        current = attempt(lifecycle.restart_due(current));
        fail(&mut lifecycle, current);
        assert!(lifecycle.lose());
        assert_eq!(lifecycle.restart_due(current), None);
        let restored = attempt(lifecycle.restore());
        assert_eq!(
            fail(&mut lifecycle, restored),
            Finished::RestartAfter(RESTART_DELAYS[0])
        );
    }

    #[test]
    fn a_running_renderer_resets_the_budget_for_the_next_loss() {
        let mut lifecycle = Lifecycle::new();
        let first = attempt(lifecycle.mount());
        fail(&mut lifecycle, first);
        let second = attempt(lifecycle.restart_due(first));
        run(&mut lifecycle, second);
        lifecycle.lose();
        let restored = attempt(lifecycle.restore());
        assert_eq!(
            fail(&mut lifecycle, restored),
            Finished::RestartAfter(RESTART_DELAYS[0])
        );
    }

    #[test]
    fn unmounting_drops_running_and_scheduled_work() {
        let mut lifecycle = Lifecycle::new();
        let first = attempt(lifecycle.mount());
        fail(&mut lifecycle, first);
        let second = attempt(lifecycle.restart_due(first));
        lifecycle.dispose();
        assert_eq!(run(&mut lifecycle, second), Finished::Stale);
        assert_eq!(lifecycle.restart_due(second), None);
        assert!(!lifecycle.lose());
        assert_eq!(lifecycle.restore(), None);
        assert_eq!(lifecycle.retry(f64::INFINITY), None);
        assert_eq!(lifecycle.mount(), None);
    }

    #[test]
    fn a_failure_reads_as_one_diagnostic_line() {
        let failure = StartFailure {
            error: InitError::new(InitStage::Surface, "canvas.getContext() returned null"),
            browser_message: Some("WebGL is disabled".into()),
            start: 4,
            canvas: (300, 150),
            dpr: 3.0,
            hidden: false,
            client: Some("sequoia-client-72779fe13d72a252".into()),
        };
        assert_eq!(
            failure.to_string(),
            "stage=surface start=4 canvas=300x150 dpr=3 visibility=visible \
             client=sequoia-client-72779fe13d72a252 browser=\"WebGL is disabled\" \
             detail=\"canvas.getContext() returned null\""
        );
    }
}
