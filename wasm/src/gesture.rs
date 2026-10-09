//! Pointer and wheel gestures that drive the map camera.
//!
//! The browser seam feeds pointer and wheel events in. [`Gestures`] tracks which contacts are
//! down and what they are doing, moves the [`Viewport`] for pans, pinches and wheel zoom, and
//! reports what the host has to act on (taps, edit strokes, picks and rubber-band selections)
//! as [`GestureEvent`]s.
//!
//! Units: pointer positions and [`GestureEvent`] coordinates are canvas CSS pixels, measured
//! from the canvas's top-left corner at full precision (fractional). The camera maps world
//! units to the same CSS pixels; the device pixel ratio only matters to the renderer.
//!
//! Invariants:
//! - Contacts are kept in press order. The two oldest drive the camera; a further contact is
//!   tracked but ignored until one of them lifts.
//! - While the driving contacts stay the same, the camera is a function of where they are now:
//!   the world point that was under their centroid stays under it, and a pinch scales by the
//!   ratio of finger separations. How many events it took to get there, or which finger moved
//!   first, does not matter.
//! - Whenever the driving contacts change, or something else moves the camera mid-gesture, the
//!   gesture holds on afresh from the current positions: the map never jumps, and a remaining
//!   finger keeps panning without a new press.
//! - Zoom limits clamp the scale but never the anchor, and reversing a clamped pinch responds
//!   at once. Separations under a fingertip count as a fingertip, so nearly touching or
//!   crossing fingers cannot zoom by an arbitrary ratio.
//! - Once a second contact has joined a press sequence, nothing in it is a tap or an edit
//!   commit. Cancelled contacts never commit anything.
//! - Touch strokes and picks commit only once the finger travels past the tap slop or lifts,
//!   so the first finger of a pinch never paints. Mouse and pen edits commit on press.

use crate::viewport::Viewport;
use crate::wheel::{TrackpadWheelClassifier, WheelSample, normalize_wheel_zoom_delta};

/// The map counts as "in use" for this long after the last gesture input, so timer-driven
/// label refreshes do not land mid-gesture.
pub const INTERACTION_SETTLE_MS: f64 = 140.0;
/// A release within this distance (CSS px) of its press is a tap rather than a drag.
const MOUSE_TAP_SLOP_PX: f64 = 4.0;
const TOUCH_TAP_SLOP_PX: f64 = 10.0;
/// A rubber band smaller than this in both directions is a click.
const SELECT_CLICK_PX: f64 = 4.0;
/// Two touch contacts are never meaningfully closer than a fingertip (CSS px). Smaller
/// separations, nearly coincident or crossing fingers, zoom as if they were this far apart.
const MIN_PINCH_SPAN_PX: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Mouse,
    Pen,
    Touch,
}

impl PointerKind {
    fn tap_slop(self) -> f64 {
        match self {
            PointerKind::Touch => TOUCH_TAP_SLOP_PX,
            PointerKind::Mouse | PointerKind::Pen => MOUSE_TAP_SLOP_PX,
        }
    }
}

/// A pointer position in canvas CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    pub id: i32,
    pub kind: PointerKind,
    pub x: f64,
    pub y: f64,
}

impl Pointer {
    fn at(&self) -> (f64, f64) {
        (self.x, self.y)
    }
}

/// What a press does, decided by the host for the first contact of a sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Drag to pan; release in place to tap.
    Pan,
    /// Paint across the map.
    Stroke,
    /// Drag out a selection rectangle.
    Select,
    /// Pick whatever is under the press.
    Pick,
    /// Already handled by the host (a minimap jump); only tracked.
    Handled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl ScreenRect {
    pub fn spanning(a: (f64, f64), b: (f64, f64)) -> Self {
        Self {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// Something the host has to act on, in canvas CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GestureEvent {
    /// A pan press released without dragging.
    Tap { x: f64, y: f64 },
    /// A paint stroke passes over this point; `start` marks the first point of a stroke.
    Stroke { x: f64, y: f64, start: bool },
    /// A pick press landed here.
    Pick { x: f64, y: f64 },
    /// The rubber band to show; `None` removes it.
    SelectPreview(Option<ScreenRect>),
    /// A rubber band was released.
    SelectBox(ScreenRect),
    /// A rubber band was released without dragging.
    SelectTap { x: f64, y: f64 },
}

#[derive(Debug, Default, PartialEq)]
pub struct GestureOutput {
    pub camera_moved: bool,
    pub events: Vec<GestureEvent>,
}

/// Holds the camera to the driving contacts: the world point that was under their centroid
/// when the grip was taken stays under the centroid, and a pinch scales with their separation.
/// The camera is recomputed from the grip on every move, never accumulated.
#[derive(Clone, Copy, Debug)]
struct Grip {
    /// Centroid, and separation for a pinch, when the grip was taken.
    from: (f64, f64),
    from_span: Option<f64>,
    /// The camera the grip was taken on; `None` until the first move binds it.
    base: Option<Viewport>,
    /// The camera this grip last produced, and the geometry it produced it for.
    produced: Option<Viewport>,
    at: (f64, f64),
    span: Option<f64>,
}

impl Grip {
    fn new(at: (f64, f64), span: Option<f64>) -> Self {
        Self {
            from: at,
            from_span: span,
            base: None,
            produced: None,
            at,
            span,
        }
    }

    /// Moves the camera for contacts now at `at` (`span` apart for a pinch). Returns whether
    /// the camera changed.
    fn follow(&mut self, camera: &mut Viewport, at: (f64, f64), span: Option<f64>) -> bool {
        if self.produced != Some(*camera) {
            // First move, or keys, a focus or the minimap moved the camera meanwhile: hold on
            // from here rather than undo it.
            self.take(*camera);
        }
        let base = self.base.unwrap_or(*camera);
        let scale = match (self.from_span, span) {
            (Some(from), Some(to)) => {
                base.scale * to.max(MIN_PINCH_SPAN_PX) / from.max(MIN_PINCH_SPAN_PX)
            }
            _ => base.scale,
        };
        let before = *camera;
        camera.anchor(base.screen_to_world(self.from.0, self.from.1), at, scale);
        self.at = at;
        self.span = span;
        if camera.scale != scale {
            // Clamped: re-take the grip so that reversing the pinch zooms straight away.
            self.take(*camera);
        }
        self.produced = Some(*camera);
        *camera != before
    }

    fn take(&mut self, camera: Viewport) {
        self.base = Some(camera);
        self.from = self.at;
        self.from_span = self.span;
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum Mode {
    #[default]
    Idle,
    Pan {
        id: i32,
        origin: (f64, f64),
        travel: f64,
        grip: Grip,
    },
    Pinch {
        a: i32,
        b: i32,
        grip: Grip,
    },
    Edit {
        id: i32,
        press: Press,
        origin: (f64, f64),
        travel: f64,
        committed: bool,
    },
    Handled {
        id: i32,
    },
}

impl Mode {
    fn involves(&self, pointer_id: i32) -> bool {
        match *self {
            Mode::Idle => false,
            Mode::Pan { id, .. } | Mode::Edit { id, .. } | Mode::Handled { id } => id == pointer_id,
            Mode::Pinch { a, b, .. } => a == pointer_id || b == pointer_id,
        }
    }
}

/// Pointer bookkeeping and camera manipulation for one map canvas.
#[derive(Debug, Default)]
pub struct Gestures {
    /// Contacts currently down, in press order.
    contacts: Vec<Pointer>,
    mode: Mode,
    /// A second contact joined (or one was cancelled) since the sequence began: releases no
    /// longer count as taps or edit commits until every contact has lifted.
    spoiled: bool,
    settle_until_ms: f64,
    wheel: TrackpadWheelClassifier,
}

impl Gestures {
    /// A contact went down. `press` says what it does if it is the first contact.
    pub fn press(&mut self, pointer: Pointer, press: Press, now_ms: f64) -> GestureOutput {
        // A press for an id that never released means its end event was lost.
        let mut out = self.drop_contact(pointer.id);
        self.settle(now_ms);
        self.contacts.push(pointer);

        if self.contacts.len() == 1 {
            self.spoiled = false;
            let origin = pointer.at();
            self.mode = match press {
                Press::Pan => Mode::Pan {
                    id: pointer.id,
                    origin,
                    travel: 0.0,
                    grip: Grip::new(origin, None),
                },
                Press::Handled => Mode::Handled { id: pointer.id },
                Press::Stroke | Press::Pick | Press::Select => {
                    let committed = press != Press::Select && pointer.kind != PointerKind::Touch;
                    if committed {
                        out.events.push(commit_event(press, origin));
                    }
                    if press == Press::Select {
                        out.events
                            .push(GestureEvent::SelectPreview(Some(ScreenRect::spanning(
                                origin, origin,
                            ))));
                    }
                    Mode::Edit {
                        id: pointer.id,
                        press,
                        origin,
                        travel: 0.0,
                        committed,
                    }
                }
            };
        } else {
            self.spoiled = true;
            if !matches!(self.mode, Mode::Pinch { .. }) {
                end_preview(self.mode, &mut out);
                self.mode = self.rebase();
            }
        }
        out
    }

    /// A contact moved. `None` when the pointer is not down (a hovering mouse).
    pub fn moved(
        &mut self,
        camera: &mut Viewport,
        pointer: Pointer,
        now_ms: f64,
    ) -> Option<GestureOutput> {
        let index = self.contacts.iter().position(|c| c.id == pointer.id)?;
        let kind = self.contacts[index].kind;
        self.contacts[index].x = pointer.x;
        self.contacts[index].y = pointer.y;
        self.settle(now_ms);

        let here = pointer.at();
        let mut out = GestureOutput::default();
        match &mut self.mode {
            Mode::Pan {
                id,
                origin,
                travel,
                grip,
            } if *id == pointer.id => {
                *travel = travel.max(distance(*origin, here));
                out.camera_moved = grip.follow(camera, here, None);
            }
            Mode::Pinch { a, b, grip } if pointer.id == *a || pointer.id == *b => {
                let (mid, span) = pair_geometry(&self.contacts, *a, *b);
                out.camera_moved = grip.follow(camera, mid, Some(span));
            }
            Mode::Edit {
                id,
                press,
                origin,
                travel,
                committed,
            } if *id == pointer.id => {
                *travel = travel.max(distance(*origin, here));
                let past_slop = *travel > kind.tap_slop();
                match press {
                    Press::Select => {
                        out.events
                            .push(GestureEvent::SelectPreview(Some(ScreenRect::spanning(
                                *origin, here,
                            ))))
                    }
                    Press::Stroke | Press::Pick => {
                        if !*committed && past_slop {
                            *committed = true;
                            out.events.push(commit_event(*press, *origin));
                        }
                        if *committed && *press == Press::Stroke {
                            out.events.push(GestureEvent::Stroke {
                                x: here.0,
                                y: here.1,
                                start: false,
                            });
                        }
                    }
                    Press::Pan | Press::Handled => {}
                }
            }
            _ => {}
        }
        Some(out)
    }

    /// A contact lifted normally.
    pub fn released(&mut self, pointer: Pointer, now_ms: f64) -> GestureOutput {
        let Some(index) = self.contacts.iter().position(|c| c.id == pointer.id) else {
            return GestureOutput::default();
        };
        let contact = self.contacts.remove(index);
        self.settle(now_ms);

        let here = pointer.at();
        let mut out = GestureOutput::default();
        if self.mode.involves(contact.id) {
            match self.mode {
                Mode::Pan { origin, travel, .. } => {
                    let travel = travel.max(distance(origin, here));
                    if !self.spoiled && travel <= contact.kind.tap_slop() {
                        out.events.push(GestureEvent::Tap {
                            x: here.0,
                            y: here.1,
                        });
                    }
                }
                Mode::Edit {
                    press,
                    origin,
                    committed,
                    ..
                } => {
                    end_preview(self.mode, &mut out);
                    if !self.spoiled {
                        match press {
                            Press::Select => {
                                let rect = ScreenRect::spanning(origin, here);
                                out.events.push(
                                    if rect.width() < SELECT_CLICK_PX
                                        && rect.height() < SELECT_CLICK_PX
                                    {
                                        GestureEvent::SelectTap {
                                            x: here.0,
                                            y: here.1,
                                        }
                                    } else {
                                        GestureEvent::SelectBox(rect)
                                    },
                                );
                            }
                            Press::Stroke | Press::Pick if !committed => {
                                out.events.push(commit_event(press, origin));
                            }
                            _ => {}
                        }
                    }
                }
                Mode::Idle | Mode::Pinch { .. } | Mode::Handled { .. } => {}
            }
            self.mode = self.rebase();
        }
        if self.contacts.is_empty() {
            self.spoiled = false;
        }
        out
    }

    /// A contact was cancelled or lost its capture. Nothing it was doing commits.
    pub fn cancelled(&mut self, pointer_id: i32, now_ms: f64) -> GestureOutput {
        if !self.contacts.iter().any(|c| c.id == pointer_id) {
            return GestureOutput::default();
        }
        self.settle(now_ms);
        self.drop_contact(pointer_id)
    }

    /// Wheel or trackpad zoom anchored at `at`.
    pub fn wheel(
        &mut self,
        camera: &mut Viewport,
        sample: WheelSample,
        at: (f64, f64),
        viewport_height: f64,
        ctrl_pinch: bool,
    ) -> bool {
        let delta =
            normalize_wheel_zoom_delta(sample, viewport_height, ctrl_pinch, &mut self.wheel);
        self.settle(sample.timestamp_ms);
        let before = *camera;
        camera.zoom_at(delta, at.0, at.1);
        *camera != before
    }

    /// Whether any contact is down.
    pub fn is_pressed(&self) -> bool {
        !self.contacts.is_empty()
    }

    /// Whether the map is being manipulated now or was moments ago.
    pub fn is_interacting(&self, now_ms: f64) -> bool {
        self.is_pressed() || now_ms < self.settle_until_ms
    }

    /// Forget every contact without committing anything, as on teardown.
    pub fn reset(&mut self) -> GestureOutput {
        let mut out = GestureOutput::default();
        end_preview(self.mode, &mut out);
        self.contacts.clear();
        self.mode = Mode::Idle;
        self.spoiled = false;
        out
    }

    fn drop_contact(&mut self, pointer_id: i32) -> GestureOutput {
        let mut out = GestureOutput::default();
        let Some(index) = self.contacts.iter().position(|c| c.id == pointer_id) else {
            return out;
        };
        self.contacts.remove(index);
        if self.mode.involves(pointer_id) {
            end_preview(self.mode, &mut out);
            self.mode = self.rebase();
        }
        // Whatever remains of this sequence must not turn into a tap.
        self.spoiled = !self.contacts.is_empty();
        out
    }

    /// The gesture the remaining contacts continue, from their current positions.
    fn rebase(&self) -> Mode {
        match self.contacts.as_slice() {
            [] => Mode::Idle,
            [only] => Mode::Pan {
                id: only.id,
                origin: only.at(),
                travel: 0.0,
                grip: Grip::new(only.at(), None),
            },
            [a, b, ..] => {
                let (mid, span) = pair_geometry(&self.contacts, a.id, b.id);
                Mode::Pinch {
                    a: a.id,
                    b: b.id,
                    grip: Grip::new(mid, Some(span)),
                }
            }
        }
    }

    fn settle(&mut self, now_ms: f64) {
        self.settle_until_ms = now_ms + INTERACTION_SETTLE_MS;
    }
}

fn commit_event(press: Press, at: (f64, f64)) -> GestureEvent {
    match press {
        Press::Pick => GestureEvent::Pick { x: at.0, y: at.1 },
        _ => GestureEvent::Stroke {
            x: at.0,
            y: at.1,
            start: true,
        },
    }
}

fn end_preview(mode: Mode, out: &mut GestureOutput) {
    if let Mode::Edit {
        press: Press::Select,
        ..
    } = mode
    {
        out.events.push(GestureEvent::SelectPreview(None));
    }
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn pair_geometry(contacts: &[Pointer], a: i32, b: i32) -> ((f64, f64), f64) {
    let find = |id: i32| {
        contacts
            .iter()
            .find(|c| c.id == id)
            .map(Pointer::at)
            .unwrap_or_default()
    };
    let (pa, pb) = (find(a), find(b));
    (((pa.0 + pb.0) * 0.5, (pa.1 + pb.1) * 0.5), distance(pa, pb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::{MAX_SCALE, MIN_SCALE};

    fn camera() -> Viewport {
        Viewport {
            offset_x: 40.0,
            offset_y: -25.0,
            scale: 0.3,
        }
    }

    fn touch(id: i32, x: f64, y: f64) -> Pointer {
        Pointer {
            id,
            kind: PointerKind::Touch,
            x,
            y,
        }
    }

    fn mouse(x: f64, y: f64) -> Pointer {
        Pointer {
            id: 1,
            kind: PointerKind::Mouse,
            x,
            y,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_world_under(camera: &Viewport, world: (f64, f64), screen: (f64, f64)) {
        let (sx, sy) = camera.world_to_screen(world.0, world.1);
        assert_close(sx, screen.0);
        assert_close(sy, screen.1);
    }

    /// Two fingers pressed symmetrically about `center`, `span` apart, then spread to `to_span`.
    fn pinch_scale_ratio(span: f64, to_span: f64) -> f64 {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let start = cam.scale;
        let (cx, cy) = (300.0, 400.0);
        gestures.press(touch(6, cx - span / 2.0, cy), Press::Pan, 0.0);
        gestures.press(touch(7, cx + span / 2.0, cy), Press::Pan, 0.0);
        gestures.moved(&mut cam, touch(6, cx - to_span / 2.0, cy), 10.0);
        gestures.moved(&mut cam, touch(7, cx + to_span / 2.0, cy), 20.0);
        cam.scale / start
    }

    #[test]
    fn pinch_scale_follows_relative_separation_for_any_initial_span() {
        assert_close(pinch_scale_ratio(60.0, 120.0), 2.0);
        assert_close(pinch_scale_ratio(120.0, 240.0), 2.0);
        assert_close(pinch_scale_ratio(300.0, 150.0), 0.5);
    }

    #[test]
    fn pinch_keeps_the_world_point_under_the_moving_midpoint() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 200.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 200.0), Press::Pan, 0.0);
        let focus = cam.screen_to_world(150.0, 200.0);

        // Spread, rotate and drag the pair across the screen.
        let steps = [
            ((90.0, 190.0), (230.0, 215.0)),
            ((140.0, 260.0), (300.0, 330.0)),
            ((260.0, 300.0), (330.0, 420.0)),
        ];
        for (n, (a, b)) in steps.into_iter().enumerate() {
            let t = n as f64 * 10.0;
            gestures.moved(&mut cam, touch(1, a.0, a.1), t);
            gestures.moved(&mut cam, touch(2, b.0, b.1), t + 5.0);
            assert_world_under(&cam, focus, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0));
        }
    }

    #[test]
    fn pinch_clamps_at_the_limits_and_reverses_without_a_dead_zone() {
        let mut gestures = Gestures::default();
        let mut cam = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: MAX_SCALE / 2.0,
        };
        gestures.press(touch(1, 100.0, 100.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 100.0), Press::Pan, 0.0);
        let focus = cam.screen_to_world(150.0, 100.0);

        // Spread to 8x the separation: clamped at the maximum, focus still under the midpoint.
        gestures.moved(&mut cam, touch(2, 900.0, 100.0), 1.0);
        assert_close(cam.scale, MAX_SCALE);
        assert_world_under(&cam, focus, (500.0, 100.0));

        // Halving the separation from there halves the scale straight away.
        gestures.moved(&mut cam, touch(2, 500.0, 100.0), 2.0);
        assert_close(cam.scale, MAX_SCALE / 2.0);

        // And the floor holds the same way.
        gestures.moved(&mut cam, touch(2, 100.5, 100.0), 3.0);
        gestures.moved(&mut cam, touch(2, 101.0, 100.0), 4.0);
        assert!(cam.scale >= MIN_SCALE);
    }

    /// Two fingers driven through `steps` (each a list of (id, x, y) moves) from a fresh press
    /// at `a`/`b`; returns the camera afterwards.
    fn drive(a: (f64, f64), b: (f64, f64), steps: &[(i32, f64, f64)]) -> Viewport {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, a.0, a.1), Press::Pan, 0.0);
        gestures.press(touch(2, b.0, b.1), Press::Pan, 0.0);
        for (n, &(id, x, y)) in steps.iter().enumerate() {
            gestures.moved(&mut cam, touch(id, x, y), n as f64);
        }
        cam
    }

    fn assert_same_camera(actual: Viewport, expected: Viewport) {
        assert_close(actual.scale, expected.scale);
        assert_close(actual.offset_x, expected.offset_x);
        assert_close(actual.offset_y, expected.offset_y);
    }

    #[test]
    fn a_pinch_depends_on_where_the_fingers_are_not_on_how_they_got_there() {
        let (a, b) = ((150.0, 420.0), (240.0, 420.0));
        let (a_end, b_end) = ((101.25, 433.5), (287.75, 380.125));
        let direct = drive(a, b, &[(1, a_end.0, a_end.1), (2, b_end.0, b_end.1)]);

        // Reverse order, one finger at a time, and a burst of fractional steps interleaving
        // both fingers, as many events between two frames would arrive.
        let reversed = drive(a, b, &[(2, b_end.0, b_end.1), (1, a_end.0, a_end.1)]);
        let mut burst = Vec::new();
        for i in 1..=200 {
            let t = f64::from(i) / 200.0;
            burst.push((1, a.0 + (a_end.0 - a.0) * t, a.1 + (a_end.1 - a.1) * t));
            burst.push((2, b.0 + (b_end.0 - b.0) * t, b.1 + (b_end.1 - b.1) * t));
        }
        let interleaved = drive(a, b, &burst);
        // Detours through nearly coincident and crossed fingers end up in the same place.
        let detour = drive(
            a,
            b,
            &[
                (1, 239.5, 420.2),
                (1, 300.0, 410.0),
                (2, 120.0, 440.0),
                (1, a_end.0, a_end.1),
                (2, b_end.0, b_end.1),
            ],
        );
        for cam in [reversed, interleaved, detour] {
            assert_same_camera(cam, direct);
        }

        let start = camera();
        let focus = start.screen_to_world((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        assert_world_under(
            &direct,
            focus,
            ((a_end.0 + b_end.0) / 2.0, (a_end.1 + b_end.1) / 2.0),
        );
        let span = |p: (f64, f64), q: (f64, f64)| (q.0 - p.0).hypot(q.1 - p.1);
        assert_close(direct.scale / start.scale, span(a_end, b_end) / span(a, b));
    }

    #[test]
    fn tiny_pinches_follow_every_fraction_of_a_pixel() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let start = cam.scale;
        gestures.press(touch(1, 150.0, 420.0), Press::Pan, 0.0);
        gestures.press(touch(2, 240.0, 420.0), Press::Pan, 0.0);
        let focus = cam.screen_to_world(195.0, 420.0);
        // Out by 0.3 px per event, then back in, as fractional pointer coordinates arrive.
        let spans = (1..=20)
            .map(|i| 90.0 + 0.3 * f64::from(i))
            .chain((0..20).rev().map(|i| 90.0 + 0.3 * f64::from(i)));
        let mut last = cam.scale;
        for (n, span) in spans.enumerate() {
            gestures.moved(&mut cam, touch(1, 195.0 - span / 2.0, 420.0), n as f64);
            gestures.moved(&mut cam, touch(2, 195.0 + span / 2.0, 420.0), n as f64);
            assert_close(cam.scale / start, span / 90.0);
            assert_world_under(&cam, focus, (195.0, 420.0));
            // No step zooms by more than the separation changed.
            assert!((cam.scale / last - 1.0).abs() <= 0.31 / 90.0 + 1e-12);
            last = cam.scale;
        }
        assert_close(cam.scale, start);
    }

    #[test]
    fn nearly_touching_and_crossing_fingers_come_back_to_the_starting_zoom() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let start = cam;
        let center = (195.0, 420.0);
        gestures.press(touch(1, center.0 - 20.0, center.1), Press::Pan, 0.0);
        gestures.press(touch(2, center.0 + 20.0, center.1), Press::Pan, 0.0);
        let focus = cam.screen_to_world(center.0, center.1);
        // 40 px apart, through 0.6 px and swapped sides, and back to 40 px.
        let halves = [10.0, 3.0, 0.3, 0.0, -0.3, -8.0, -20.0, -8.0, 0.0, 8.0, 20.0];
        for (n, half) in halves.into_iter().enumerate() {
            gestures.moved(&mut cam, touch(1, center.0 - half, center.1), n as f64);
            gestures.moved(&mut cam, touch(2, center.0 + half, center.1), n as f64);
            assert!(cam.scale >= start.scale * MIN_PINCH_SPAN_PX / 40.0 - 1e-12);
            assert!(cam.scale <= start.scale + 1e-12);
            assert_world_under(&cam, focus, center);
        }
        assert_same_camera(cam, start);
    }

    #[test]
    fn a_camera_moved_by_the_host_mid_gesture_is_kept() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 100.0), Press::Pan, 0.0);
        gestures.moved(&mut cam, touch(1, 120.0, 100.0), 1.0);
        // Keys zoom in meanwhile; the drag carries on from there instead of undoing it.
        cam.zoom_at(-300.0, 50.0, 50.0);
        let zoomed = cam;
        let under = cam.screen_to_world(120.0, 100.0);
        gestures.moved(&mut cam, touch(1, 150.0, 130.0), 2.0);
        assert_close(cam.scale, zoomed.scale);
        assert_world_under(&cam, under, (150.0, 130.0));
    }

    #[test]
    fn lifting_the_first_pinch_finger_hands_over_without_a_jump() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(6, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(7, 160.0, 300.0), Press::Pan, 10.0);
        gestures.moved(&mut cam, touch(6, 80.0, 310.0), 20.0);
        gestures.moved(&mut cam, touch(7, 200.0, 290.0), 20.0);
        let after_pinch = cam;
        gestures.released(touch(6, 80.0, 310.0), 30.0);
        assert_eq!(cam, after_pinch);
        let under = cam.screen_to_world(200.0, 290.0);
        gestures.moved(&mut cam, touch(7, 230.0, 250.0), 40.0);
        assert_close(cam.scale, after_pinch.scale);
        assert_world_under(&cam, under, (230.0, 250.0));
    }

    #[test]
    fn pinch_and_pan_cycles_keep_the_map_glued_to_the_fingers() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let mut a = (150.0, 400.0);
        let mut b = (250.0, 400.0);
        gestures.press(touch(1, a.0, a.1), Press::Pan, 0.0);
        let mut next_id = 2;
        let mut t = 0.0;
        for cycle in 0..4 {
            // Second finger joins, pinch and drag.
            gestures.press(touch(next_id, b.0, b.1), Press::Pan, t);
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            let focus = cam.screen_to_world(mid.0, mid.1);
            for step in 1..=10 {
                t += 1.0;
                let k = f64::from(step);
                a = (a.0 - 1.5, a.1 + 0.5 * f64::from(cycle));
                b = (b.0 + 2.5 + 0.1 * k, b.1 - 0.75);
                gestures.moved(&mut cam, touch(1, a.0, a.1), t);
                gestures.moved(&mut cam, touch(next_id, b.0, b.1), t);
                assert_world_under(&cam, focus, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0));
            }
            // Second finger lifts; the first pans on alone.
            let before_lift = cam;
            gestures.released(touch(next_id, b.0, b.1), t);
            assert_eq!(cam, before_lift);
            let under = cam.screen_to_world(a.0, a.1);
            for _ in 0..5 {
                t += 1.0;
                a = (a.0 + 3.0, a.1 - 2.0);
                gestures.moved(&mut cam, touch(1, a.0, a.1), t);
                assert_world_under(&cam, under, a);
                assert_close(cam.scale, before_lift.scale);
            }
            b = (a.0 + 90.0, a.1 + 5.0);
            next_id += 1;
        }
    }

    #[test]
    fn lifting_one_pinch_finger_resumes_panning_with_the_other() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(6, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(7, 160.0, 300.0), Press::Pan, 10.0);
        gestures.moved(&mut cam, touch(7, 220.0, 300.0), 20.0);
        let after_pinch = cam;

        let out = gestures.released(touch(7, 220.0, 300.0), 30.0);
        assert!(out.events.is_empty());
        assert_eq!(cam, after_pinch, "lifting a finger must not move the map");

        let out = gestures
            .moved(&mut cam, touch(6, 150.0, 300.0), 40.0)
            .expect("surviving finger is still tracked");
        assert!(out.camera_moved);
        assert_close(cam.offset_x - after_pinch.offset_x, 50.0);
        assert_close(cam.offset_y, after_pinch.offset_y);
        assert_close(cam.scale, after_pinch.scale);

        // The sequence had two fingers, so its last release is not a tap.
        assert!(
            gestures
                .released(touch(6, 150.0, 300.0), 50.0)
                .events
                .is_empty()
        );
        assert!(!gestures.is_pressed());
    }

    #[test]
    fn a_third_contact_is_ignored_until_a_pinching_finger_lifts() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 100.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 100.0), Press::Pan, 0.0);
        gestures.press(touch(3, 400.0, 400.0), Press::Pan, 0.0);
        let before = cam;

        let out = gestures
            .moved(&mut cam, touch(3, 600.0, 600.0), 1.0)
            .unwrap();
        assert!(!out.camera_moved);
        assert_eq!(cam, before);

        // Contact 1 lifts: 2 and 3 take over from where they are, without a jump.
        gestures.released(touch(1, 100.0, 100.0), 2.0);
        assert_eq!(cam, before);
        let focus = cam.screen_to_world(400.0, 350.0);
        gestures.moved(&mut cam, touch(3, 1000.0, 1100.0), 3.0);
        // Separation of (200,100)-(600,600) is ~640; to (1000,1100) it doubled.
        let span = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0).hypot(b.1 - a.1);
        let ratio = span((200.0, 100.0), (1000.0, 1100.0)) / span((200.0, 100.0), (600.0, 600.0));
        assert_close(cam.scale / before.scale, ratio);
        assert_world_under(&cam, focus, (600.0, 600.0));
    }

    #[test]
    fn short_presses_are_taps_and_drags_are_not() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Pan, 0.0);
        gestures.moved(&mut cam, mouse(12.0, 13.0), 1.0);
        assert_eq!(
            gestures.released(mouse(12.0, 13.0), 2.0).events,
            [GestureEvent::Tap { x: 12.0, y: 13.0 }]
        );

        gestures.press(mouse(10.0, 10.0), Press::Pan, 3.0);
        gestures.moved(&mut cam, mouse(30.0, 10.0), 4.0);
        gestures.moved(&mut cam, mouse(11.0, 10.0), 5.0);
        assert!(gestures.released(mouse(11.0, 10.0), 6.0).events.is_empty());

        // Fingers get a wider slop than a mouse.
        gestures.press(touch(4, 10.0, 10.0), Press::Pan, 7.0);
        gestures.moved(&mut cam, touch(4, 17.0, 16.0), 8.0);
        assert_eq!(gestures.released(touch(4, 17.0, 16.0), 9.0).events.len(), 1);
    }

    #[test]
    fn two_finger_taps_select_nothing() {
        let mut gestures = Gestures::default();
        gestures.press(touch(1, 10.0, 10.0), Press::Pan, 0.0);
        gestures.press(touch(2, 60.0, 10.0), Press::Pan, 1.0);
        assert!(
            gestures
                .released(touch(2, 60.0, 10.0), 2.0)
                .events
                .is_empty()
        );
        assert!(
            gestures
                .released(touch(1, 10.0, 10.0), 3.0)
                .events
                .is_empty()
        );

        // A fresh press afterwards is a normal tap again.
        gestures.press(touch(3, 10.0, 10.0), Press::Pan, 4.0);
        assert_eq!(gestures.released(touch(3, 10.0, 10.0), 5.0).events.len(), 1);
    }

    #[test]
    fn mouse_strokes_commit_on_press_and_follow_the_pointer() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let pressed = gestures.press(mouse(5.0, 5.0), Press::Stroke, 0.0);
        assert_eq!(
            pressed.events,
            [GestureEvent::Stroke {
                x: 5.0,
                y: 5.0,
                start: true
            }]
        );
        let moved = gestures.moved(&mut cam, mouse(6.0, 5.0), 1.0).unwrap();
        assert!(!moved.camera_moved);
        assert_eq!(
            moved.events,
            [GestureEvent::Stroke {
                x: 6.0,
                y: 5.0,
                start: false
            }]
        );
        assert!(gestures.released(mouse(6.0, 5.0), 2.0).events.is_empty());
    }

    #[test]
    fn touch_strokes_wait_for_slop_or_release() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        assert!(
            gestures
                .press(touch(1, 50.0, 50.0), Press::Stroke, 0.0)
                .events
                .is_empty()
        );
        assert!(
            gestures
                .moved(&mut cam, touch(1, 53.0, 50.0), 1.0)
                .unwrap()
                .events
                .is_empty()
        );
        let crossed = gestures.moved(&mut cam, touch(1, 70.0, 50.0), 2.0).unwrap();
        assert_eq!(
            crossed.events,
            [
                GestureEvent::Stroke {
                    x: 50.0,
                    y: 50.0,
                    start: true
                },
                GestureEvent::Stroke {
                    x: 70.0,
                    y: 50.0,
                    start: false
                },
            ]
        );
        assert!(
            gestures
                .released(touch(1, 70.0, 50.0), 3.0)
                .events
                .is_empty()
        );

        // A tap paints its spot on release.
        gestures.press(touch(2, 80.0, 80.0), Press::Stroke, 4.0);
        assert_eq!(
            gestures.released(touch(2, 81.0, 80.0), 5.0).events,
            [GestureEvent::Stroke {
                x: 80.0,
                y: 80.0,
                start: true
            }]
        );
    }

    #[test]
    fn a_pinch_never_paints_with_its_first_finger_and_ends_strokes() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 50.0, 50.0), Press::Stroke, 0.0);
        let second = gestures.press(touch(2, 150.0, 50.0), Press::Stroke, 5.0);
        assert!(second.events.is_empty());
        let moved = gestures.moved(&mut cam, touch(1, 0.0, 50.0), 6.0).unwrap();
        assert!(moved.camera_moved);
        assert!(moved.events.is_empty());
        assert!(
            gestures
                .released(touch(2, 150.0, 50.0), 7.0)
                .events
                .is_empty()
        );
        // The remaining finger pans; it does not resume painting.
        let panned = gestures.moved(&mut cam, touch(1, 20.0, 50.0), 8.0).unwrap();
        assert!(panned.camera_moved);
        assert!(panned.events.is_empty());
        assert!(
            gestures
                .released(touch(1, 20.0, 50.0), 9.0)
                .events
                .is_empty()
        );

        // A committed mouse stroke stops as soon as a pinch takes over.
        gestures.press(mouse(5.0, 5.0), Press::Stroke, 10.0);
        gestures.press(touch(9, 50.0, 50.0), Press::Pan, 11.0);
        let moved = gestures.moved(&mut cam, mouse(8.0, 5.0), 12.0).unwrap();
        assert!(moved.events.is_empty());
    }

    #[test]
    fn cancelled_or_pinched_selections_never_commit() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        gestures.moved(&mut cam, mouse(90.0, 70.0), 1.0);
        assert_eq!(
            gestures.cancelled(1, 2.0).events,
            [GestureEvent::SelectPreview(None)]
        );
        assert!(!gestures.is_pressed());
        // A late release for the cancelled pointer is ignored.
        assert!(gestures.released(mouse(90.0, 70.0), 3.0).events.is_empty());

        gestures.press(touch(1, 10.0, 10.0), Press::Select, 4.0);
        gestures.moved(&mut cam, touch(1, 60.0, 60.0), 5.0);
        let pinch = gestures.press(touch(2, 200.0, 200.0), Press::Pan, 6.0);
        assert_eq!(pinch.events, [GestureEvent::SelectPreview(None)]);
        assert!(
            gestures
                .released(touch(1, 60.0, 60.0), 7.0)
                .events
                .is_empty()
        );
        assert!(
            gestures
                .released(touch(2, 200.0, 200.0), 8.0)
                .events
                .is_empty()
        );
    }

    #[test]
    fn selections_release_as_boxes_or_clicks() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        gestures.moved(&mut cam, mouse(90.0, 70.0), 1.0);
        assert_eq!(
            gestures.released(mouse(90.0, 70.0), 2.0).events,
            [
                GestureEvent::SelectPreview(None),
                GestureEvent::SelectBox(ScreenRect {
                    left: 10.0,
                    top: 10.0,
                    right: 90.0,
                    bottom: 70.0
                }),
            ]
        );

        gestures.press(mouse(10.0, 10.0), Press::Select, 3.0);
        assert_eq!(
            gestures.released(mouse(12.0, 11.0), 4.0).events,
            [
                GestureEvent::SelectPreview(None),
                GestureEvent::SelectTap { x: 12.0, y: 11.0 },
            ]
        );
    }

    #[test]
    fn touch_picks_commit_on_release_unless_a_pinch_starts() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 30.0, 30.0), Press::Pick, 0.0);
        assert_eq!(
            gestures.released(touch(1, 31.0, 30.0), 1.0).events,
            [GestureEvent::Pick { x: 30.0, y: 30.0 }]
        );

        gestures.press(touch(1, 30.0, 30.0), Press::Pick, 2.0);
        gestures.press(touch(2, 90.0, 30.0), Press::Pan, 3.0);
        assert!(
            gestures
                .released(touch(1, 30.0, 30.0), 4.0)
                .events
                .is_empty()
        );
        assert!(gestures.cancelled(2, 5.0).events.is_empty());

        // A mouse pick lands on press and does not pan.
        let pressed = gestures.press(mouse(30.0, 30.0), Press::Pick, 6.0);
        assert_eq!(pressed.events, [GestureEvent::Pick { x: 30.0, y: 30.0 }]);
        let before = cam;
        gestures.moved(&mut cam, mouse(80.0, 30.0), 7.0);
        assert_eq!(cam, before);
    }

    #[test]
    fn a_cancelled_pinch_finger_leaves_a_pan_that_never_taps() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 100.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 100.0), Press::Pan, 1.0);
        gestures.cancelled(2, 2.0);
        let before = cam;
        gestures.moved(&mut cam, touch(1, 130.0, 90.0), 3.0);
        assert_close(cam.offset_x - before.offset_x, 30.0);
        assert_close(cam.offset_y - before.offset_y, -10.0);
        assert!(
            gestures
                .released(touch(1, 130.0, 90.0), 4.0)
                .events
                .is_empty()
        );
    }

    #[test]
    fn a_repeated_press_id_replaces_the_stale_contact() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 10.0, 10.0), Press::Pan, 0.0);
        // The release was lost; the same id comes down again elsewhere.
        gestures.press(touch(1, 300.0, 300.0), Press::Pan, 1.0);
        assert_eq!(gestures.contacts.len(), 1);
        // It is not mistaken for a two-finger pinch.
        let before = cam;
        gestures.moved(&mut cam, touch(1, 320.0, 300.0), 2.0);
        assert_close(cam.offset_x - before.offset_x, 20.0);
        assert_close(cam.scale, before.scale);
    }

    #[test]
    fn handled_presses_neither_pan_nor_tap() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Handled, 0.0);
        let before = cam;
        let moved = gestures.moved(&mut cam, mouse(50.0, 10.0), 1.0).unwrap();
        assert!(!moved.camera_moved);
        assert_eq!(cam, before);
        assert!(gestures.released(mouse(10.0, 10.0), 2.0).events.is_empty());
    }

    #[test]
    fn interaction_settles_shortly_after_the_last_input() {
        let mut gestures = Gestures::default();
        assert!(!gestures.is_interacting(0.0));
        gestures.press(mouse(10.0, 10.0), Press::Pan, 100.0);
        assert!(gestures.is_interacting(10_000.0));
        gestures.released(mouse(10.0, 10.0), 200.0);
        assert!(gestures.is_interacting(200.0 + INTERACTION_SETTLE_MS - 1.0));
        assert!(!gestures.is_interacting(200.0 + INTERACTION_SETTLE_MS));
    }

    #[test]
    fn hovering_pointers_are_not_part_of_a_gesture() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        assert!(gestures.moved(&mut cam, mouse(10.0, 10.0), 0.0).is_none());
        assert!(!gestures.is_interacting(0.0));
    }

    #[test]
    fn teardown_drops_previews_without_committing() {
        let mut gestures = Gestures::default();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        assert_eq!(gestures.reset().events, [GestureEvent::SelectPreview(None)]);
        assert!(!gestures.is_pressed());
    }

    #[test]
    fn wheel_zoom_keeps_the_cursor_anchored() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let focus = cam.screen_to_world(320.0, 240.0);
        let sample = WheelSample {
            delta_x: 0.0,
            delta_y: -120.0,
            delta_mode: 0,
            timestamp_ms: 50.0,
        };
        assert!(gestures.wheel(&mut cam, sample, (320.0, 240.0), 800.0, false));
        assert!(cam.scale > camera().scale);
        assert_world_under(&cam, focus, (320.0, 240.0));
        assert!(gestures.is_interacting(50.0));
    }
}
