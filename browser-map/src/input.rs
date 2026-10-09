//! Browser pointer and wheel events, adapted onto the engine's gesture state machine.
//!
//! This is the seam between DOM events and [`Gestures`]: it converts events to canvas
//! coordinates, decides what a press does (minimap jump, edit tool, pan), manages pointer
//! capture, and turns gesture output into [`MapEvent`]s by hit-testing territories.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use sequoia_map_engine::gesture::{GestureEvent, Pointer, PointerKind, Press, ScreenRect};
use sequoia_map_engine::wheel::WheelSample;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, PointerEvent, WheelEvent};

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
        event.prevent_default();
        let pointer = pointer_of(event);
        self.map.pointer.set((pointer.x, pointer.y));
        let press = self.press_for(event, pointer);
        let output = self
            .state
            .borrow_mut()
            .gestures
            .press(pointer, press, js_sys::Date::now());
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
                .moved(view, pointer, js_sys::Date::now());
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
            .released(pointer, js_sys::Date::now());
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
            .cancelled(event.pointer_id(), js_sys::Date::now());
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
        let at = (f64::from(event.offset_x()), f64::from(event.offset_y()));
        self.map.pointer.set(at);
        let sample = WheelSample {
            delta_x: event.delta_x(),
            delta_y: event.delta_y(),
            delta_mode: event.delta_mode(),
            timestamp_ms: js_sys::Date::now(),
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

fn pointer_of(event: &PointerEvent) -> Pointer {
    Pointer {
        id: event.pointer_id(),
        kind: match event.pointer_type().as_str() {
            "touch" => PointerKind::Touch,
            "pen" => PointerKind::Pen,
            _ => PointerKind::Mouse,
        },
        x: f64::from(event.offset_x()),
        y: f64::from(event.offset_y()),
    }
}

fn event_canvas(event: &PointerEvent) -> Option<HtmlCanvasElement> {
    event
        .current_target()
        .and_then(|target| target.dyn_into::<HtmlCanvasElement>().ok())
}
