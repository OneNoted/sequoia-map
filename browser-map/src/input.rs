//! Browser pointer and wheel events, adapted onto the engine's gesture state machine.
//!
//! This is the seam between DOM events and [`Gestures`]: it converts events to canvas CSS
//! pixels, decides what a press does (minimap jump, edit tool, pan), manages pointer capture,
//! and turns gesture output into [`MapEvent`]s by hit-testing territories. The gesture
//! semantics themselves live in the engine.
//!
//! Positions are `clientX`/`clientY` minus the canvas's client rect, both read as the doubles
//! browsers report. `offsetX`/`offsetY` would do, but web-sys's stable getters truncate them
//! to whole pixels, which turns sub-pixel finger motion into one-pixel zoom steps. Times are
//! event time stamps (monotonic, like `performance.now()`).
//!
//! Touch presses are deliberately not `preventDefault`ed: `touch-action: none` already keeps
//! the browser's own panning and zooming off the canvas, and Firefox before 159 stops
//! delivering every other finger's `pointermove`/`pointerup` to the page once the first
//! finger's `pointerdown` has been default-prevented (Mozilla bug 1524251), which froze the
//! second finger of every pinch where it landed.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use sequoia_map_engine::gesture::{GestureEvent, Pointer, PointerKind, Press, ScreenRect};
use sequoia_map_engine::wheel::WheelSample;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{HtmlCanvasElement, MouseEvent, PointerEvent, WheelEvent};

use crate::canvas::MapState;
use crate::render_loop::RenderScheduler;
use crate::{BrowserMap, EditMode, MapEvent};

#[derive(Clone)]
pub(crate) struct MapInput {
    pub(crate) map: BrowserMap,
    pub(crate) on_event: Callback<MapEvent>,
    pub(crate) state: Rc<RefCell<MapState>>,
    pub(crate) scheduler: Rc<RenderScheduler>,
    /// The selection rectangle being dragged out, in canvas CSS pixels.
    pub(crate) select_box: RwSignal<Option<ScreenRect>>,
}

impl MapInput {
    pub(crate) fn pointer_down(&self, event: &PointerEvent) {
        let pointer = pointer_of(event);
        if presses_prevent_default(pointer.kind) {
            event.prevent_default();
        }
        self.map.pointer.set((pointer.x, pointer.y));
        let press = self.press_for(event, pointer);
        let output = self
            .state
            .borrow_mut()
            .gestures
            .press(pointer, press, event.time_stamp());
        if let Some(canvas) = event_canvas(event) {
            let _ = canvas.set_pointer_capture(pointer.id);
        }
        self.dispatch(output.events, event.shift_key());
        self.scheduler.mark_dirty();
    }

    pub(crate) fn pointer_move(&self, event: &PointerEvent) {
        let pointer = pointer_of(event);
        self.map.pointer.set((pointer.x, pointer.y));
        let mut output = None;
        self.map.camera.update_if(|view| {
            output = self
                .state
                .borrow_mut()
                .gestures
                .moved(view, pointer, event.time_stamp());
            output.as_ref().is_some_and(|output| output.camera_moved)
        });
        match output {
            Some(output) => {
                self.dispatch(output.events, event.shift_key());
                self.scheduler.mark_dirty();
            }
            None => self.hover_at(pointer.x, pointer.y),
        }
    }

    pub(crate) fn pointer_up(&self, event: &PointerEvent) {
        let pointer = pointer_of(event);
        let mut output = self
            .state
            .borrow_mut()
            .gestures
            .released(pointer, event.time_stamp());
        // Only the primary button taps; others just pan.
        if event.button() != 0 {
            output
                .events
                .retain(|event| !matches!(event, GestureEvent::Tap { .. }));
        }
        self.dispatch(output.events, event.shift_key());
        self.scheduler.mark_dirty();
    }

    /// `pointercancel` and `lostpointercapture`: the contact is gone without completing.
    /// After a normal release the pointer is no longer tracked and this does nothing.
    pub(crate) fn pointer_cancel(&self, event: &PointerEvent) {
        let output = self
            .state
            .borrow_mut()
            .gestures
            .cancelled(event.pointer_id(), event.time_stamp());
        self.dispatch(output.events, false);
        self.scheduler.mark_dirty();
    }

    pub(crate) fn pointer_leave(&self) {
        if self.state.borrow().gestures.is_pressed() {
            return;
        }
        if self.map.hovered.with_untracked(Option::is_some) {
            self.map.hovered.set(None);
            self.on_event.run(MapEvent::Hover(None));
        }
    }

    pub(crate) fn wheel(&self, event: &WheelEvent) {
        event.prevent_default();
        let at = canvas_position(event);
        self.map.pointer.set(at);
        let sample = WheelSample {
            delta_x: event.delta_x(),
            delta_y: event.delta_y(),
            delta_mode: event.delta_mode(),
            timestamp_ms: event.time_stamp(),
        };
        let (_, height) = self.map.camera.canvas_size();
        self.map.camera.update_if(|view| {
            self.state
                .borrow_mut()
                .gestures
                .wheel(view, sample, at, height, event.ctrl_key())
        });
        self.scheduler.mark_dirty();
    }

    fn press_for(&self, event: &PointerEvent, pointer: Pointer) -> Press {
        // Only the first contact of a sequence decides; later ones join a pinch.
        if event.button() != 0 || self.state.borrow().gestures.is_pressed() {
            return Press::Pan;
        }
        let minimap = self.state.borrow().minimap_layout(&self.map);
        if let Some(minimap) = minimap
            && minimap.contains(pointer.x, pointer.y)
        {
            self.map
                .camera
                .center_on(minimap.screen_to_world(pointer.x, pointer.y));
            return Press::Handled;
        }
        let on_territory = || self.territory_at(pointer.x, pointer.y).is_some();
        match self.map.inputs.edit.get_untracked() {
            EditMode::Navigate => Press::Pan,
            EditMode::Select => Press::Select,
            EditMode::Stroke if on_territory() => Press::Stroke,
            EditMode::Pick if on_territory() => Press::Pick,
            EditMode::Stroke | EditMode::Pick => Press::Pan,
        }
    }

    fn dispatch(&self, events: Vec<GestureEvent>, shift: bool) {
        for event in events {
            let map_event = match event {
                GestureEvent::Tap { x, y } => Some(MapEvent::Tap {
                    territory: self.territory_at(x, y),
                    shift,
                }),
                GestureEvent::Stroke { x, y, start } => {
                    let hit = self.territory_at(x, y);
                    let mut state = self.state.borrow_mut();
                    if start {
                        state.stroke_hit = None;
                    }
                    if hit == state.stroke_hit {
                        None
                    } else {
                        state.stroke_hit.clone_from(&hit);
                        hit.map(MapEvent::Stroke)
                    }
                }
                GestureEvent::Pick { x, y } => self
                    .territory_at(x, y)
                    .map(|territory| MapEvent::Pick { territory, shift }),
                GestureEvent::SelectPreview(rect) => {
                    self.select_box.set(rect);
                    None
                }
                GestureEvent::SelectBox(rect) => Some(MapEvent::BoxSelect {
                    territories: self.territories_in(rect),
                    shift,
                }),
                GestureEvent::SelectTap { x, y } => Some(MapEvent::SelectTap {
                    territory: self.territory_at(x, y),
                    shift,
                }),
            };
            if let Some(map_event) = map_event {
                self.on_event.run(map_event);
            }
        }
    }

    fn hover_at(&self, x: f64, y: f64) {
        let hit = self.territory_at(x, y);
        if self.map.hovered.with_untracked(|hovered| *hovered != hit) {
            self.map.hovered.set(hit.clone());
            self.on_event.run(MapEvent::Hover(hit));
        }
    }

    fn territory_at(&self, x: f64, y: f64) -> Option<String> {
        let (wx, wy) = self.map.camera.get_untracked().screen_to_world(x, y);
        self.state.borrow().grid.find_at(wx, wy)
    }

    fn territories_in(&self, rect: ScreenRect) -> Vec<String> {
        let view = self.map.camera.get_untracked();
        let (ax, ay) = view.screen_to_world(rect.left, rect.top);
        let (bx, by) = view.screen_to_world(rect.right, rect.bottom);
        let (left, top, right, bottom) = (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by));
        let mut hits: Vec<String> = self.map.inputs.territories.with_untracked(|territories| {
            territories
                .iter()
                .filter(|(_, territory)| {
                    let region = &territory.territory.location;
                    f64::from(region.left()) <= right
                        && f64::from(region.right()) >= left
                        && f64::from(region.top()) <= bottom
                        && f64::from(region.bottom()) >= top
                })
                .map(|(name, _)| name.clone())
                .collect()
        });
        hits.sort();
        hits
    }
}

#[wasm_bindgen]
extern "C" {
    /// A `MouseEvent` read through getters that keep the doubles browsers report; web-sys's
    /// stable `client_x`/`client_y` return `i32`.
    #[wasm_bindgen(extends = MouseEvent)]
    type FractionalMouseEvent;
    #[wasm_bindgen(method, getter = clientX)]
    fn client_x(this: &FractionalMouseEvent) -> f64;
    #[wasm_bindgen(method, getter = clientY)]
    fn client_y(this: &FractionalMouseEvent) -> f64;
}

/// Whether a press of this kind cancels the browser's default action. Mouse presses do, so a
/// drag never selects page text or moves focus. Touch presses must not: see the module docs.
/// Pens keep their browser defaults too; `touch-action` covers them like fingers.
fn presses_prevent_default(kind: PointerKind) -> bool {
    kind == PointerKind::Mouse
}

fn pointer_of(event: &PointerEvent) -> Pointer {
    let (x, y) = canvas_position(event);
    Pointer {
        id: event.pointer_id(),
        kind: match event.pointer_type().as_str() {
            "touch" => PointerKind::Touch,
            "pen" => PointerKind::Pen,
            _ => PointerKind::Mouse,
        },
        x,
        y,
    }
}

/// The event's position in canvas CSS pixels. Captured pointers report positions outside
/// the canvas too.
fn canvas_position(event: &MouseEvent) -> (f64, f64) {
    let precise = event.unchecked_ref::<FractionalMouseEvent>();
    let client = (precise.client_x(), precise.client_y());
    let origin = event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .map(|canvas| {
            let rect = canvas.get_bounding_client_rect();
            (rect.left(), rect.top())
        })
        .unwrap_or_default();
    (client.0 - origin.0, client.1 - origin.1)
}

fn event_canvas(event: &PointerEvent) -> Option<HtmlCanvasElement> {
    event
        .current_target()
        .and_then(|target| target.dyn_into::<HtmlCanvasElement>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mouse_presses_cancel_browser_defaults() {
        assert!(presses_prevent_default(PointerKind::Mouse));
        assert!(!presses_prevent_default(PointerKind::Touch));
        assert!(!presses_prevent_default(PointerKind::Pen));
    }
}
