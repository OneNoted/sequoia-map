//! Polling that starts the moment there is something to ask for.

use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::future::select;
use leptos::prelude::*;

/// When a poll loop may ask. A loop that waits on a fixed timer while it cannot ask, before
/// territories arrive or during a History visit, answers seconds or minutes after it could.
/// [`Self::open`] returns the moment it may ask, and [`Self::sleep`] ends early whenever asking
/// becomes possible again.
pub(crate) struct PollGate {
    ready: Memo<bool>,
    opened: UnboundedReceiver<()>,
}

impl PollGate {
    /// Create it in the component, where it can watch `ready`, before spawning its loop.
    pub(crate) fn new(ready: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let ready = Memo::new(move |_| ready());
        let (sender, opened) = unbounded();
        Effect::new(move || {
            if ready.get() {
                let _ = sender.unbounded_send(());
            }
        });
        Self { ready, opened }
    }

    /// Returns once the loop may ask, at once when it already may.
    pub(crate) async fn open(&mut self) {
        while !self.ready.get_untracked() {
            if self.opened.next().await.is_none() {
                return;
            }
        }
        while self.opened.try_recv().is_ok() {}
    }

    /// Waits `interval`, or less when asking becomes possible again meanwhile.
    pub(crate) async fn sleep(&mut self, interval: Duration) {
        select(
            Box::pin(gloo_timers::future::sleep(interval)),
            self.opened.next(),
        )
        .await;
    }
}
