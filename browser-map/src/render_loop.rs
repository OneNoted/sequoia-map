use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

/// Batches render requests via `requestAnimationFrame`.
///
/// Call `mark_dirty()` whenever state changes. The render function
/// fires at most once per vsync (~16ms), coalescing all dirty marks.
///
/// When the render function returns `true`, the scheduler automatically
/// schedules another frame (for animations that need continuous rendering).
pub struct RenderScheduler {
    inner: Rc<Inner>,
}

struct Inner {
    window: Option<web_sys::Window>,
    dirty: Cell<bool>,
    scheduled: Cell<bool>,
    raf_id: Cell<Option<i32>>,
    callback: RefCell<Option<Closure<dyn FnMut()>>>,
}

impl RenderScheduler {
    /// Create a scheduler. `render_fn` returns `true` if animations are active
    /// and another frame should be scheduled automatically.
    pub fn new(render_fn: impl Fn() -> bool + 'static) -> Self {
        let inner = Rc::new(Inner {
            window: web_sys::window(),
            dirty: Cell::new(false),
            scheduled: Cell::new(false),
            raf_id: Cell::new(None),
            callback: RefCell::new(None),
        });

        let inner_cb = inner.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            inner_cb.scheduled.set(false);
            inner_cb.raf_id.set(None);
            if inner_cb.dirty.get() {
                inner_cb.dirty.set(false);
                let needs_more = render_fn();

                if needs_more {
                    inner_cb.dirty.set(true);
                    if !inner_cb.scheduled.get() {
                        inner_cb.scheduled.set(true);
                        let cb_ref = inner_cb.callback.borrow();
                        if let Some(ref cb) = *cb_ref {
                            let Some(window) = inner_cb.window.as_ref() else {
                                inner_cb.scheduled.set(false);
                                return;
                            };
                            match window.request_animation_frame(cb.as_ref().unchecked_ref()) {
                                Ok(id) => inner_cb.raf_id.set(Some(id)),
                                Err(_) => inner_cb.scheduled.set(false),
                            }
                        }
                    }
                }
            }
        });
        *inner.callback.borrow_mut() = Some(cb);

        Self { inner }
    }

    /// Mark the scene as needing a repaint. Cheap — just sets a flag
    /// and schedules one rAF if none is pending.
    pub fn mark_dirty(&self) {
        self.inner.dirty.set(true);
        if !self.inner.scheduled.get() {
            self.inner.scheduled.set(true);
            let cb_ref = self.inner.callback.borrow();
            if let Some(ref cb) = *cb_ref {
                let Some(window) = self.inner.window.as_ref() else {
                    self.inner.scheduled.set(false);
                    return;
                };
                match window.request_animation_frame(cb.as_ref().unchecked_ref()) {
                    Ok(id) => self.inner.raf_id.set(Some(id)),
                    Err(_) => self.inner.scheduled.set(false),
                }
            }
        }
    }
}

impl Drop for RenderScheduler {
    fn drop(&mut self) {
        if let Some(raf_id) = self.inner.raf_id.replace(None)
            && let Some(window) = self.inner.window.as_ref()
        {
            let _ = window.cancel_animation_frame(raf_id);
        }
        self.inner.scheduled.set(false);
        self.inner.dirty.set(false);
        // Break the callback->inner reference cycle on teardown.
        self.inner.callback.borrow_mut().take();
    }
}
