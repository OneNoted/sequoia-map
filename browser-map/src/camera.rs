//! The reactive camera handle hosts use to read and move the map view.

use leptos::prelude::*;
use sequoia_map_engine::viewport::Viewport;
use sequoia_shared::Region;

/// World units of surroundings kept in view when focusing a territory.
const FOCUS_CONTEXT_WORLD: f64 = 200.0;

/// The map's view transform plus the canvas size it applies to, so callers can fit and zoom
/// without knowing how large the map is on screen.
#[derive(Clone, Copy)]
pub struct MapCamera {
    view: RwSignal<Viewport>,
    canvas_size: StoredValue<Option<(f64, f64)>>,
}

impl MapCamera {
    pub(crate) fn new(view: Viewport) -> Self {
        Self {
            view: RwSignal::new(view),
            canvas_size: StoredValue::new(None),
        }
    }

    /// The current view, tracked.
    pub fn get(&self) -> Viewport {
        self.view.get()
    }

    pub fn get_untracked(&self) -> Viewport {
        self.view.get_untracked()
    }

    pub fn track(&self) {
        self.view.track();
    }

    pub fn set(&self, view: Viewport) {
        self.view.set(view);
    }

    pub fn update(&self, change: impl FnOnce(&mut Viewport)) {
        self.view.update(change);
    }

    /// Applies `change` and notifies only if it moved the camera.
    pub(crate) fn update_if(&self, change: impl FnOnce(&mut Viewport) -> bool) -> bool {
        let mut moved = false;
        self.view.maybe_update(|view| {
            moved = change(view);
            moved
        });
        moved
    }

    /// Fits the world rectangle (min x, min z, max x, max z) into the canvas.
    pub fn fit(&self, bounds: (f64, f64, f64, f64)) {
        let (width, height) = self.canvas_size();
        let (min_x, min_z, max_x, max_z) = bounds;
        self.view
            .update(|view| view.fit_bounds(min_x, min_z, max_x, max_z, width, height));
    }

    /// Brings a territory into view with some of its surroundings.
    pub fn focus(&self, region: &Region) {
        self.fit((
            f64::from(region.left()) - FOCUS_CONTEXT_WORLD,
            f64::from(region.top()) - FOCUS_CONTEXT_WORLD,
            f64::from(region.right()) + FOCUS_CONTEXT_WORLD,
            f64::from(region.bottom()) + FOCUS_CONTEXT_WORLD,
        ));
    }

    /// Zooms about the middle of the canvas (positive `delta` zooms out).
    pub fn zoom_at_center(&self, delta: f64) {
        let (width, height) = self.canvas_size();
        self.view
            .update(|view| view.zoom_at(delta, width / 2.0, height / 2.0));
    }

    /// Centres the world point in the canvas without zooming.
    pub(crate) fn center_on(&self, world: (f64, f64)) {
        let (width, height) = self.canvas_size();
        self.view
            .update(|view| view.center_on(world, width, height));
    }

    /// The map canvas size in CSS pixels; the window size before the canvas has laid out.
    pub fn canvas_size(&self) -> (f64, f64) {
        self.canvas_size
            .get_value()
            .unwrap_or_else(window_inner_size)
    }

    pub(crate) fn set_canvas_size(&self, size: (f64, f64)) {
        self.canvas_size.set_value(Some(size));
    }
}

fn window_inner_size() -> (f64, f64) {
    let Some(window) = web_sys::window() else {
        return (1200.0, 800.0);
    };
    let read = |value: Result<wasm_bindgen::JsValue, _>, fallback| {
        value.ok().and_then(|v| v.as_f64()).unwrap_or(fallback)
    };
    (
        read(window.inner_width(), 1200.0),
        read(window.inner_height(), 800.0),
    )
}
