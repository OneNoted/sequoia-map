use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

/// Batches render requests via `requestAnimationFrame`.
///
/// Call `mark_dirty()` whenever state changes. The render function runs at most once per
/// display frame, with the frame's `requestAnimationFrame` time stamp, coalescing all dirty
/// marks. When it returns `true` (something is still moving), another frame follows.
pub struct RenderScheduler {
    inner: Rc<Inner>,
}

/// The `requestAnimationFrame` callback, given the frame's time stamp.
type FrameCallback = Closure<dyn FnMut(f64)>;

struct Inner {
    window: Option<web_sys::Window>,
    dirty: Cell<bool>,
    raf_id: Cell<Option<i32>>,
    callback: RefCell<Option<FrameCallback>>,
}

impl Inner {
    /// Asks for the next frame unless one is already coming.
    fn request(&self) {
        if self.raf_id.get().is_some() {
            return;
        }
        let (Some(window), Some(callback)) = (self.window.as_ref(), &*self.callback.borrow())
        else {
            return;
        };
        if let Ok(id) = window.request_animation_frame(callback.as_ref().unchecked_ref()) {
            self.raf_id.set(Some(id));
        }
    }
}

impl RenderScheduler {
    pub fn new(render_fn: impl Fn(f64) -> bool + 'static) -> Self {
        let inner = Rc::new(Inner {
            window: web_sys::window(),
            dirty: Cell::new(false),
            raf_id: Cell::new(None),
            callback: RefCell::new(None),
        });
        let frame = inner.clone();
        let callback = FrameCallback::new(move |frame_ms: f64| {
            frame.raf_id.set(None);
            if frame.dirty.replace(false) && render_fn(frame_ms) {
                frame.dirty.set(true);
                frame.request();
            }
        });
        *inner.callback.borrow_mut() = Some(callback);
        Self { inner }
    }

    /// Marks the scene as needing a repaint: sets a flag and asks for one frame if none is
    /// pending.
    pub fn mark_dirty(&self) {
        self.inner.dirty.set(true);
        self.inner.request();
    }
}

impl Drop for RenderScheduler {
    fn drop(&mut self) {
        if let Some(raf_id) = self.inner.raf_id.take()
            && let Some(window) = self.inner.window.as_ref()
        {
            let _ = window.cancel_animation_frame(raf_id);
        }
        self.inner.dirty.set(false);
        // Break the callback->inner reference cycle on teardown.
        self.inner.callback.borrow_mut().take();
    }
}
