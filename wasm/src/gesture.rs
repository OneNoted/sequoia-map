//! Pointer and wheel gestures that drive the map camera.
//!
//! The browser seam feeds pointer and wheel events in. [`Gestures`] tracks which contacts are
//! down and what they are doing, and reports what the host has to act on (taps, edit strokes,
//! picks and rubber-band selections) as [`GestureEvent`]s. Pans and pinches move the
//! [`Viewport`] once per drawn frame, in [`Gestures::frame`]; wheel zoom moves it as each
//! notch arrives.
//!
//! Units: pointer positions and [`GestureEvent`] coordinates are canvas CSS pixels, measured
//! from the canvas's top-left corner at full precision (fractional). The camera maps world
//! units to the same CSS pixels; the device pixel ratio only matters to the renderer. Times
//! are event time stamps and frame times on one monotonic clock (`performance.now()`).
//!
//! Invariants:
//! - Contacts are kept in press order. The two oldest drive the camera; a further contact is
//!   tracked but ignored until one of them lifts.
//! - While the driving contacts stay the same, the camera is a function of where they were at
//!   the frame's sampling time: the world point that was under their centroid stays under it,
//!   and a pinch scales by the ratio of finger separations. How many events it took to get
//!   there, or which finger moved first, does not matter.
//! - Each frame reads the contacts at its own time less the typical (median) lateness of recent
//!   reports, between the positions they reported, rather than at whichever report last
//!   arrived. Browsers deliver moves at their own phase relative to frames (Gecko's differs
//!   from Blink's), so the latest report can be a frame fresher or staler from one frame to
//!   the next; reading at a steady time moves the map by the same amount on every frame of an
//!   even drag. A frame whose reports are later than usual carries a contact on along its
//!   last motion, by at most [`MAX_PREDICTION_MS`] and half its report interval, and only
//!   while it is still reporting; a contact that stops is shown where it stopped. Reading
//!   times never go back, and never trail the newest report by more than
//!   [`MAX_REPORT_DELAY_MS`]. On average the map is no later than the newest report.
//! - Whenever the driving contacts change, or something else moves the camera mid-gesture, the
//!   gesture holds on afresh from where the map shows them: the map never jumps, and a
//!   remaining finger keeps panning without a new press. A pan released mid-drag finishes,
//!   over its last few milliseconds of reports, where the finger lifted. A pinch that loses a
//!   finger is finished with both fingers where they last reported, so its outcome never
//!   depends on whether a frame ran before the finger lifted; the other finger pans on from
//!   there.
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
/// The most a frame reads the contacts behind their newest report. Reports later than this
/// count as a contact holding still.
pub const MAX_REPORT_DELAY_MS: f64 = 24.0;
/// The furthest a frame carries a contact on past its newest report.
pub const MAX_PREDICTION_MS: f64 = 4.0;
/// A release within this distance (CSS px) of its press is a tap rather than a drag.
const MOUSE_TAP_SLOP_PX: f64 = 4.0;
const TOUCH_TAP_SLOP_PX: f64 = 10.0;
/// A rubber band smaller than this in both directions is a click.
const SELECT_CLICK_PX: f64 = 4.0;
/// Two touch contacts are never meaningfully closer than a fingertip (CSS px). Smaller
/// separations, nearly coincident or crossing fingers, zoom as if they were this far apart.
const MIN_PINCH_SPAN_PX: f64 = 16.0;
/// Reports kept per contact: several frames of a touchscreen's or mouse's report rate.
const TRAIL_LEN: usize = 8;
/// Frames with new reports whose lateness sets the reading delay: about half a second.
const LATENESS_WINDOW: usize = 32;
/// Reports closer together than this give no usable velocity; ones further apart are a
/// contact that paused, not one in motion.
const PREDICTION_INTERVALS_MS: std::ops::RangeInclusive<f64> = 2.0..=20.0;

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

/// What one frame did to the camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameMotion {
    pub moved: bool,
    /// The contacts reported positions later than this frame sampled: draw another frame.
    pub pending: bool,
}

/// One reported position.
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    at_ms: f64,
    x: f64,
    y: f64,
}

/// A contact's recent reports, oldest first, so a frame can read where it was at any time
/// between them.
#[derive(Clone, Copy, Debug)]
struct Trail {
    samples: [Sample; TRAIL_LEN],
    len: usize,
}

impl Trail {
    fn new(at_ms: f64, (x, y): (f64, f64)) -> Self {
        let mut samples = [Sample::default(); TRAIL_LEN];
        samples[0] = Sample { at_ms, x, y };
        Self { samples, len: 1 }
    }

    fn push(&mut self, at_ms: f64, (x, y): (f64, f64)) {
        // A report never predates the one before it.
        let at_ms = at_ms.max(self.newest().at_ms);
        if self.len == TRAIL_LEN {
            self.samples.copy_within(1.., 0);
            self.len -= 1;
        }
        self.samples[self.len] = Sample { at_ms, x, y };
        self.len += 1;
    }

    fn newest(&self) -> Sample {
        self.samples[self.len - 1]
    }

    /// Where the contact was at `at_ms`: on the line between the reports around it, or at the
    /// oldest report before them.
    fn at(&self, at_ms: f64) -> (f64, f64) {
        let samples = &self.samples[..self.len];
        match samples.iter().position(|s| s.at_ms > at_ms) {
            None => (samples[self.len - 1].x, samples[self.len - 1].y),
            Some(0) => (samples[0].x, samples[0].y),
            Some(i) => {
                let (a, b) = (samples[i - 1], samples[i]);
                let t = (at_ms - a.at_ms) / (b.at_ms - a.at_ms);
                (a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
            }
        }
    }

    /// Where the contact is about at `at_ms` for a frame drawn at `frame_ms`: [`Self::at`],
    /// carried on past the newest report along its last motion while the contact is still
    /// reporting, by at most [`MAX_PREDICTION_MS`] and half its report interval.
    fn predicted(&self, at_ms: f64, frame_ms: f64) -> (f64, f64) {
        let newest = self.newest();
        if at_ms <= newest.at_ms || self.len < 2 {
            return self.at(at_ms);
        }
        let previous = self.samples[self.len - 2];
        let interval = newest.at_ms - previous.at_ms;
        let reporting = frame_ms - newest.at_ms <= 2.0 * interval;
        if !PREDICTION_INTERVALS_MS.contains(&interval) || !reporting {
            return (newest.x, newest.y);
        }
        let ahead = (at_ms - newest.at_ms)
            .min(MAX_PREDICTION_MS)
            .min(interval / 2.0)
            / interval;
        (
            newest.x + (newest.x - previous.x) * ahead,
            newest.y + (newest.y - previous.y) * ahead,
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Contact {
    pointer: Pointer,
    trail: Trail,
    /// Where the last frame showed it, for a gesture that takes over from there.
    shown: (f64, f64),
}

/// When each frame reads the driving contacts: the frame time less the median lateness of
/// recent reports, so the readings are evenly spaced even when the reports are not.
#[derive(Clone, Copy, Debug)]
struct SampleClock {
    /// How far the newest report trailed each recent frame that had a new move.
    lateness: [f64; LATENESS_WINDOW],
    recorded: usize,
    /// The newest report time the last frame saw.
    seen_ms: f64,
    sampled_ms: f64,
}

impl Default for SampleClock {
    fn default() -> Self {
        Self {
            lateness: [0.0; LATENESS_WINDOW],
            recorded: 0,
            seen_ms: f64::NEG_INFINITY,
            sampled_ms: f64::NEG_INFINITY,
        }
    }
}

impl SampleClock {
    /// The time a frame drawn at `frame_ms` reads the contacts, given until when every
    /// driving contact's position is known; `moving` when the newest report is a move.
    fn sample(&mut self, frame_ms: f64, known_ms: f64, moving: bool) -> f64 {
        if known_ms > self.seen_ms {
            self.seen_ms = known_ms;
            // A press is not motion, and how late it arrived says nothing about moves. A report
            // made after the frame's time stamp (before its callback ran) is early: frames
            // then read that much after their time stamp, which is still a real report.
            if moving {
                self.lateness[self.recorded % LATENESS_WINDOW] =
                    (frame_ms - known_ms).clamp(-MAX_REPORT_DELAY_MS, MAX_REPORT_DELAY_MS);
                self.recorded += 1;
            }
        }
        let mut recent = self.lateness;
        let recent = &mut recent[..self.recorded.min(LATENESS_WINDOW)];
        recent.sort_by(f64::total_cmp);
        let delay = recent.get(recent.len() / 2).copied().unwrap_or(0.0);
        self.sampled_ms = (frame_ms - delay)
            .max(self.sampled_ms)
            .clamp(known_ms - MAX_REPORT_DELAY_MS, known_ms + MAX_PREDICTION_MS);
        self.sampled_ms
    }
}

/// Holds the camera to the driving contacts: the world point that was under their centroid
/// when the grip was taken stays under the centroid, and a pinch scales with their separation.
/// The camera is recomputed from the grip on every frame, never accumulated.
#[derive(Clone, Copy, Debug)]
struct Grip {
    /// Centroid, and separation for a pinch, when the grip was taken.
    from: (f64, f64),
    from_span: Option<f64>,
    /// The camera the grip was taken on; `None` until the first frame binds it.
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
            // First frame, or keys, a focus or the minimap moved the camera meanwhile: hold
            // on from here rather than undo it.
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

/// A released pan finishing the drag its frames had not caught up with yet.
#[derive(Clone, Copy, Debug)]
struct Glide {
    grip: Grip,
    trail: Trail,
}

/// A pinch that lost a finger, finished by the next frame with both fingers where they last
/// reported, before the gesture goes on.
#[derive(Clone, Copy, Debug)]
struct Finish {
    grip: Grip,
    at: (f64, f64),
    span: f64,
}

/// Pointer bookkeeping and camera manipulation for one map canvas.
#[derive(Debug, Default)]
pub struct Gestures {
    /// Contacts currently down, in press order.
    contacts: Vec<Contact>,
    mode: Mode,
    /// A second contact joined (or one was cancelled) since the sequence began: releases no
    /// longer count as taps or edit commits until every contact has lifted.
    spoiled: bool,
    settle_until_ms: f64,
    clock: SampleClock,
    glide: Option<Glide>,
    finish: Option<Finish>,
    wheel: TrackpadWheelClassifier,
}

impl Gestures {
    /// A contact went down. `press` says what it does if it is the first contact.
    pub fn press(&mut self, pointer: Pointer, press: Press, now_ms: f64) -> Vec<GestureEvent> {
        // A press for an id that never released means its end event was lost.
        let mut out = self.drop_contact(pointer.id);
        self.settle(now_ms);
        if self.contacts.is_empty() {
            self.clock = SampleClock::default();
            self.glide = None;
            self.finish = None;
        }
        self.contacts.push(Contact {
            pointer,
            trail: Trail::new(now_ms, pointer.at()),
            shown: pointer.at(),
        });

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
                        out.push(commit_event(press, origin));
                    }
                    if press == Press::Select {
                        out.push(GestureEvent::SelectPreview(Some(ScreenRect::spanning(
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
                self.rebase();
            }
        }
        out
    }

    /// A contact moved. `None` when the pointer is not down (a hovering mouse). A pan or
    /// pinch follows the move in the next [`Self::frame`].
    pub fn moved(&mut self, pointer: Pointer, now_ms: f64) -> Option<Vec<GestureEvent>> {
        let contact = self
            .contacts
            .iter_mut()
            .find(|c| c.pointer.id == pointer.id)?;
        let kind = contact.pointer.kind;
        contact.pointer.x = pointer.x;
        contact.pointer.y = pointer.y;
        contact.trail.push(now_ms, pointer.at());
        self.settle(now_ms);

        let here = pointer.at();
        let mut out = Vec::new();
        match &mut self.mode {
            Mode::Pan {
                id, origin, travel, ..
            } if *id == pointer.id => {
                *travel = travel.max(distance(*origin, here));
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
                    Press::Select => out.push(GestureEvent::SelectPreview(Some(
                        ScreenRect::spanning(*origin, here),
                    ))),
                    Press::Stroke | Press::Pick => {
                        if !*committed && past_slop {
                            *committed = true;
                            out.push(commit_event(*press, *origin));
                        }
                        if *committed && *press == Press::Stroke {
                            out.push(GestureEvent::Stroke {
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

    /// Moves the camera for a frame drawn at `frame_ms`: a pan or pinch follows its contacts
    /// as they were at this frame's reading time, and a released pan finishes where it lifted.
    pub fn frame(&mut self, camera: &mut Viewport, frame_ms: f64) -> FrameMotion {
        let finished = self
            .finish
            .take()
            .is_some_and(|mut finish| finish.grip.follow(camera, finish.at, Some(finish.span)));
        let motion = self.follow(camera, frame_ms);
        FrameMotion {
            moved: finished || motion.moved,
            pending: motion.pending,
        }
    }

    fn follow(&mut self, camera: &mut Viewport, frame_ms: f64) -> FrameMotion {
        let (driving, grip): ([Option<usize>; 2], &mut Grip) = match &mut self.mode {
            Mode::Pan { id, grip, .. } => ([index_of(&self.contacts, *id), None], grip),
            Mode::Pinch { a, b, grip } => (
                [index_of(&self.contacts, *a), index_of(&self.contacts, *b)],
                grip,
            ),
            Mode::Idle => match &mut self.glide {
                Some(glide) => {
                    // The released finger's reports are complete: no carrying on past them.
                    let newest = glide.trail.newest().at_ms;
                    let at_ms = self.clock.sample(frame_ms, newest, false);
                    let at = glide.trail.at(at_ms);
                    let moved = glide.grip.follow(camera, at, None);
                    let pending = at_ms < newest;
                    if !pending {
                        self.glide = None;
                    }
                    return FrameMotion { moved, pending };
                }
                None => return FrameMotion::default(),
            },
            Mode::Edit { .. } | Mode::Handled { .. } => return FrameMotion::default(),
        };
        let trails = driving.map(|index| index.map(|i| self.contacts[i].trail));
        let reported = || trails.iter().flatten().map(|trail| trail.newest().at_ms);
        let Some(newest_ms) = reported().reduce(f64::max) else {
            return FrameMotion::default();
        };
        // Read no later than every contact's position is known. Reports are never later
        // than the reading delay's limit, so one silent since before then is holding still.
        let known_ms = reported()
            .map(|at_ms| at_ms.max(frame_ms - MAX_REPORT_DELAY_MS))
            .fold(newest_ms, f64::min);
        let moving = trails.iter().flatten().any(|trail| trail.len > 1);
        let at_ms = self.clock.sample(frame_ms, known_ms, moving);
        let shown = trails.map(|trail| trail.map(|trail| trail.predicted(at_ms, frame_ms)));
        let (at, span) = match shown {
            [Some(a), Some(b)] => (midpoint(a, b), Some(distance(a, b))),
            [Some(only), None] | [None, Some(only)] => (only, None),
            [None, None] => return FrameMotion::default(),
        };
        let moved = grip.follow(camera, at, span);
        for (index, position) in driving.into_iter().zip(shown) {
            if let (Some(index), Some(position)) = (index, position) {
                self.contacts[index].shown = position;
            }
        }
        // Another frame catches up with reports read later, or settles a contact carried on
        // past its newest report.
        let carried = trails.iter().zip(shown).any(|(trail, shown)| {
            trail.zip(shown).is_some_and(|(trail, shown)| {
                let newest = trail.newest();
                shown != (newest.x, newest.y) && at_ms > newest.at_ms
            })
        });
        let pending = at_ms < newest_ms || carried;
        FrameMotion { moved, pending }
    }

    /// A contact lifted normally.
    pub fn released(&mut self, pointer: Pointer, now_ms: f64) -> Vec<GestureEvent> {
        let Some(index) = self
            .contacts
            .iter()
            .position(|c| c.pointer.id == pointer.id)
        else {
            return Vec::new();
        };
        let mut contact = self.contacts.remove(index);
        contact.trail.push(now_ms, pointer.at());
        self.settle(now_ms);

        let here = pointer.at();
        let mut out = Vec::new();
        if self.mode.involves(contact.pointer.id) {
            match self.mode {
                Mode::Pan {
                    origin,
                    travel,
                    grip,
                    ..
                } => {
                    let travel = travel.max(distance(origin, here));
                    if !self.spoiled && travel <= contact.pointer.kind.tap_slop() {
                        out.push(GestureEvent::Tap {
                            x: here.0,
                            y: here.1,
                        });
                    }
                    if self.contacts.is_empty() {
                        self.glide = Some(Glide {
                            grip,
                            trail: contact.trail,
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
                                out.push(
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
                                out.push(commit_event(press, origin));
                            }
                            _ => {}
                        }
                    }
                }
                Mode::Pinch { .. } => self.finish_pinch(contact.pointer.id, here),
                Mode::Idle | Mode::Handled { .. } => {}
            }
            self.rebase();
        }
        if self.contacts.is_empty() {
            self.spoiled = false;
        }
        out
    }

    /// A contact was cancelled or lost its capture. Nothing it was doing commits.
    pub fn cancelled(&mut self, pointer_id: i32, now_ms: f64) -> Vec<GestureEvent> {
        if !self.contacts.iter().any(|c| c.pointer.id == pointer_id) {
            return Vec::new();
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
    pub fn reset(&mut self) -> Vec<GestureEvent> {
        let mut out = Vec::new();
        end_preview(self.mode, &mut out);
        self.contacts.clear();
        self.mode = Mode::Idle;
        self.glide = None;
        self.finish = None;
        self.spoiled = false;
        out
    }

    fn drop_contact(&mut self, pointer_id: i32) -> Vec<GestureEvent> {
        let mut out = Vec::new();
        let Some(index) = self
            .contacts
            .iter()
            .position(|c| c.pointer.id == pointer_id)
        else {
            return out;
        };
        let newest = self.contacts.remove(index).trail.newest();
        if self.mode.involves(pointer_id) {
            end_preview(self.mode, &mut out);
            self.finish_pinch(pointer_id, (newest.x, newest.y));
            self.rebase();
        }
        // Whatever remains of this sequence must not turn into a tap.
        self.spoiled = !self.contacts.is_empty();
        out
    }

    /// Keeps, for the next frame, the pinch as the fingers last reported it when finger
    /// `lifted` leaves, last at `last`: all the movement they delivered as a pinch counts as
    /// pinch. The other finger then pans on from its own last report.
    fn finish_pinch(&mut self, lifted: i32, last: (f64, f64)) {
        let Mode::Pinch { a, b, grip } = self.mode else {
            return;
        };
        let other = if lifted == a { b } else { a };
        if let Some(survivor) = self.contacts.iter_mut().find(|c| c.pointer.id == other) {
            let newest = survivor.trail.newest();
            let reported = (newest.x, newest.y);
            // The pinch has used its reports; a pan must not replay them.
            survivor.trail = Trail::new(newest.at_ms, reported);
            survivor.shown = reported;
            self.finish = Some(Finish {
                grip,
                at: midpoint(last, reported),
                span: distance(last, reported),
            });
        }
    }

    /// Hands the gesture to the remaining contacts, from where the last frame showed the ones
    /// that drove it and from where the others are now.
    fn rebase(&mut self) {
        let was_driving = |id: i32| match self.mode {
            Mode::Pan { id: pan, .. } => id == pan,
            Mode::Pinch { a, b, .. } => id == a || id == b,
            _ => false,
        };
        let joining: Vec<usize> = self
            .contacts
            .iter()
            .take(2)
            .enumerate()
            .filter(|(_, contact)| !was_driving(contact.pointer.id))
            .map(|(index, _)| index)
            .collect();
        for index in joining {
            // Its earlier moves drove nothing, so the gesture must not replay them.
            let contact = &mut self.contacts[index];
            let newest = contact.trail.newest();
            contact.trail = Trail::new(newest.at_ms, (newest.x, newest.y));
            contact.shown = (newest.x, newest.y);
        }
        let at = |contact: &Contact| contact.shown;
        self.mode = match self.contacts.as_slice() {
            [] => Mode::Idle,
            [only] => Mode::Pan {
                id: only.pointer.id,
                origin: only.pointer.at(),
                travel: 0.0,
                grip: Grip::new(at(only), None),
            },
            [a, b, ..] => {
                let (pa, pb) = (at(a), at(b));
                Mode::Pinch {
                    a: a.pointer.id,
                    b: b.pointer.id,
                    grip: Grip::new(midpoint(pa, pb), Some(distance(pa, pb))),
                }
            }
        };
    }

    fn settle(&mut self, now_ms: f64) {
        self.settle_until_ms = now_ms + INTERACTION_SETTLE_MS;
    }
}

fn index_of(contacts: &[Contact], id: i32) -> Option<usize> {
    contacts.iter().position(|c| c.pointer.id == id)
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

fn end_preview(mode: Mode, out: &mut Vec<GestureEvent>) {
    if let Mode::Edit {
        press: Press::Select,
        ..
    } = mode
    {
        out.push(GestureEvent::SelectPreview(None));
    }
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn midpoint(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_close;
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

    /// Moves a contact, then draws a frame late enough to show the contacts where they are.
    fn drag(
        gestures: &mut Gestures,
        cam: &mut Viewport,
        pointer: Pointer,
        at_ms: f64,
    ) -> Option<Vec<GestureEvent>> {
        let out = gestures.moved(pointer, at_ms);
        gestures.frame(cam, at_ms + MAX_REPORT_DELAY_MS);
        out
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
        drag(
            &mut gestures,
            &mut cam,
            touch(6, cx - to_span / 2.0, cy),
            10.0,
        );
        drag(
            &mut gestures,
            &mut cam,
            touch(7, cx + to_span / 2.0, cy),
            20.0,
        );
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
            drag(&mut gestures, &mut cam, touch(1, a.0, a.1), t);
            drag(&mut gestures, &mut cam, touch(2, b.0, b.1), t + 5.0);
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
        drag(&mut gestures, &mut cam, touch(2, 900.0, 100.0), 1.0);
        assert_close(cam.scale, MAX_SCALE);
        assert_world_under(&cam, focus, (500.0, 100.0));

        // Halving the separation from there halves the scale straight away.
        drag(&mut gestures, &mut cam, touch(2, 500.0, 100.0), 2.0);
        assert_close(cam.scale, MAX_SCALE / 2.0);

        // And the floor holds the same way.
        drag(&mut gestures, &mut cam, touch(2, 100.5, 100.0), 3.0);
        drag(&mut gestures, &mut cam, touch(2, 101.0, 100.0), 4.0);
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
            drag(&mut gestures, &mut cam, touch(id, x, y), n as f64);
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
            drag(
                &mut gestures,
                &mut cam,
                touch(1, 195.0 - span / 2.0, 420.0),
                n as f64,
            );
            drag(
                &mut gestures,
                &mut cam,
                touch(2, 195.0 + span / 2.0, 420.0),
                n as f64,
            );
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
            drag(
                &mut gestures,
                &mut cam,
                touch(1, center.0 - half, center.1),
                n as f64,
            );
            drag(
                &mut gestures,
                &mut cam,
                touch(2, center.0 + half, center.1),
                n as f64,
            );
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
        drag(&mut gestures, &mut cam, touch(1, 120.0, 100.0), 1.0);
        // Keys zoom in meanwhile; the drag carries on from there instead of undoing it.
        cam.zoom_at(-300.0, 50.0, 50.0);
        let zoomed = cam;
        let under = cam.screen_to_world(120.0, 100.0);
        drag(&mut gestures, &mut cam, touch(1, 150.0, 130.0), 2.0);
        assert_close(cam.scale, zoomed.scale);
        assert_world_under(&cam, under, (150.0, 130.0));
    }

    #[test]
    fn lifting_the_first_pinch_finger_hands_over_without_a_jump() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(6, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(7, 160.0, 300.0), Press::Pan, 10.0);
        drag(&mut gestures, &mut cam, touch(6, 80.0, 310.0), 20.0);
        drag(&mut gestures, &mut cam, touch(7, 200.0, 290.0), 20.0);
        let after_pinch = cam;
        gestures.released(touch(6, 80.0, 310.0), 30.0);
        assert_eq!(cam, after_pinch);
        let under = cam.screen_to_world(200.0, 290.0);
        drag(&mut gestures, &mut cam, touch(7, 230.0, 250.0), 40.0);
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
        let mut t = 0.0;
        for (next_id, cycle) in (2..).zip(0..4) {
            // Second finger joins, pinch and drag.
            gestures.press(touch(next_id, b.0, b.1), Press::Pan, t);
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            let focus = cam.screen_to_world(mid.0, mid.1);
            for step in 1..=10 {
                t += 1.0;
                let k = f64::from(step);
                a = (a.0 - 1.5, a.1 + 0.5 * f64::from(cycle));
                b = (b.0 + 2.5 + 0.1 * k, b.1 - 0.75);
                drag(&mut gestures, &mut cam, touch(1, a.0, a.1), t);
                drag(&mut gestures, &mut cam, touch(next_id, b.0, b.1), t);
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
                drag(&mut gestures, &mut cam, touch(1, a.0, a.1), t);
                assert_world_under(&cam, under, a);
                assert_close(cam.scale, before_lift.scale);
            }
            b = (a.0 + 90.0, a.1 + 5.0);
        }
    }

    /// Two fingers 100 px apart about (150, 300); finger 2 spreads to x = 300 and leaves, by
    /// `leave`, before any frame shows the spread. Returns the camera after the next frame,
    /// then after finger 1 pans 20 px right.
    fn spread_then_leave(leave: impl FnOnce(&mut Gestures)) -> (Viewport, Viewport) {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 300.0), Press::Pan, 0.0);
        gestures.frame(&mut cam, 30.0);
        gestures.moved(touch(2, 300.0, 300.0), 40.0);
        leave(&mut gestures);
        gestures.frame(&mut cam, 60.0);
        let spread = cam;
        drag(&mut gestures, &mut cam, touch(1, 120.0, 300.0), 70.0);
        (spread, cam)
    }

    #[test]
    fn a_pinch_finger_that_leaves_before_the_next_frame_keeps_its_last_move() {
        let focus = camera().screen_to_world(150.0, 300.0);
        for (spread, panned) in [
            spread_then_leave(|g| {
                g.released(touch(2, 300.0, 300.0), 45.0);
            }),
            spread_then_leave(|g| {
                g.cancelled(2, 45.0);
            }),
        ] {
            assert_close(spread.scale / camera().scale, 2.0);
            assert_world_under(&spread, focus, (200.0, 300.0));
            // The remaining finger then pans on from there.
            assert_close(panned.scale, spread.scale);
            assert_close(panned.offset_x - spread.offset_x, 20.0);
        }
    }

    /// Fingers at x = 100 and 200 move to 50 and 250, and finger 2 lifts, with a frame between
    /// the moves and the lift or not; then finger 1 pans 20 px right.
    fn pinch_then_lift(frame_before_lift: bool) -> (Viewport, Viewport) {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(2, 200.0, 300.0), Press::Pan, 0.0);
        gestures.frame(&mut cam, 30.0);
        gestures.moved(touch(1, 50.0, 300.0), 40.0);
        gestures.moved(touch(2, 250.0, 300.0), 41.0);
        if frame_before_lift {
            gestures.frame(&mut cam, 44.0);
        }
        gestures.released(touch(2, 250.0, 300.0), 46.0);
        for frame_ms in [60.0, 77.0, 94.0] {
            gestures.frame(&mut cam, frame_ms);
        }
        let pinched = cam;
        drag(&mut gestures, &mut cam, touch(1, 70.0, 300.0), 100.0);
        (pinched, cam)
    }

    #[test]
    fn a_pinch_ends_the_same_whether_or_not_a_frame_ran_before_a_finger_lifted() {
        let focus = camera().screen_to_world(150.0, 300.0);
        let (framed, framed_then_panned) = pinch_then_lift(true);
        let (lifted, lifted_then_panned) = pinch_then_lift(false);
        for (pinched, panned) in [(framed, framed_then_panned), (lifted, lifted_then_panned)] {
            assert_close(pinched.scale / camera().scale, 2.0);
            assert_world_under(&pinched, focus, (150.0, 300.0));
            assert_close(panned.scale, pinched.scale);
            assert_close(panned.offset_x - pinched.offset_x, 20.0);
        }
        assert_same_camera(lifted, framed);
    }

    #[test]
    fn lifting_one_pinch_finger_resumes_panning_with_the_other() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(touch(6, 100.0, 300.0), Press::Pan, 0.0);
        gestures.press(touch(7, 160.0, 300.0), Press::Pan, 10.0);
        drag(&mut gestures, &mut cam, touch(7, 220.0, 300.0), 20.0);
        let after_pinch = cam;

        let out = gestures.released(touch(7, 220.0, 300.0), 30.0);
        assert!(out.is_empty());
        assert_eq!(cam, after_pinch, "lifting a finger must not move the map");

        let out = drag(&mut gestures, &mut cam, touch(6, 150.0, 300.0), 40.0)
            .expect("surviving finger is still tracked");
        assert!(out.is_empty());
        assert_close(cam.offset_x - after_pinch.offset_x, 50.0);
        assert_close(cam.offset_y, after_pinch.offset_y);
        assert_close(cam.scale, after_pinch.scale);

        // The sequence had two fingers, so its last release is not a tap.
        assert!(gestures.released(touch(6, 150.0, 300.0), 50.0).is_empty());
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

        drag(&mut gestures, &mut cam, touch(3, 600.0, 600.0), 1.0);
        assert_eq!(cam, before);

        // Contact 1 lifts: 2 and 3 take over from where they are, without a jump.
        gestures.released(touch(1, 100.0, 100.0), 2.0);
        assert_eq!(cam, before);
        let focus = cam.screen_to_world(400.0, 350.0);
        drag(&mut gestures, &mut cam, touch(3, 1000.0, 1100.0), 3.0);
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
        drag(&mut gestures, &mut cam, mouse(12.0, 13.0), 1.0);
        assert_eq!(
            gestures.released(mouse(12.0, 13.0), 2.0),
            [GestureEvent::Tap { x: 12.0, y: 13.0 }]
        );

        gestures.press(mouse(10.0, 10.0), Press::Pan, 3.0);
        drag(&mut gestures, &mut cam, mouse(30.0, 10.0), 4.0);
        drag(&mut gestures, &mut cam, mouse(11.0, 10.0), 5.0);
        assert!(gestures.released(mouse(11.0, 10.0), 6.0).is_empty());

        // Fingers get a wider slop than a mouse.
        gestures.press(touch(4, 10.0, 10.0), Press::Pan, 7.0);
        drag(&mut gestures, &mut cam, touch(4, 17.0, 16.0), 8.0);
        assert_eq!(gestures.released(touch(4, 17.0, 16.0), 9.0).len(), 1);
    }

    #[test]
    fn two_finger_taps_select_nothing() {
        let mut gestures = Gestures::default();
        gestures.press(touch(1, 10.0, 10.0), Press::Pan, 0.0);
        gestures.press(touch(2, 60.0, 10.0), Press::Pan, 1.0);
        assert!(gestures.released(touch(2, 60.0, 10.0), 2.0).is_empty());
        assert!(gestures.released(touch(1, 10.0, 10.0), 3.0).is_empty());

        // A fresh press afterwards is a normal tap again.
        gestures.press(touch(3, 10.0, 10.0), Press::Pan, 4.0);
        assert_eq!(gestures.released(touch(3, 10.0, 10.0), 5.0).len(), 1);
    }

    #[test]
    fn mouse_strokes_commit_on_press_and_follow_the_pointer() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let pressed = gestures.press(mouse(5.0, 5.0), Press::Stroke, 0.0);
        assert_eq!(
            pressed,
            [GestureEvent::Stroke {
                x: 5.0,
                y: 5.0,
                start: true
            }]
        );
        let moved = drag(&mut gestures, &mut cam, mouse(6.0, 5.0), 1.0).unwrap();
        assert_eq!(cam, camera(), "painting never pans");
        assert_eq!(
            moved,
            [GestureEvent::Stroke {
                x: 6.0,
                y: 5.0,
                start: false
            }]
        );
        assert!(gestures.released(mouse(6.0, 5.0), 2.0).is_empty());
    }

    #[test]
    fn touch_strokes_wait_for_slop_or_release() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        assert!(
            gestures
                .press(touch(1, 50.0, 50.0), Press::Stroke, 0.0)
                .is_empty()
        );
        assert!(
            drag(&mut gestures, &mut cam, touch(1, 53.0, 50.0), 1.0)
                .unwrap()
                .is_empty()
        );
        let crossed = drag(&mut gestures, &mut cam, touch(1, 70.0, 50.0), 2.0).unwrap();
        assert_eq!(
            crossed,
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
        assert!(gestures.released(touch(1, 70.0, 50.0), 3.0).is_empty());

        // A tap paints its spot on release.
        gestures.press(touch(2, 80.0, 80.0), Press::Stroke, 4.0);
        assert_eq!(
            gestures.released(touch(2, 81.0, 80.0), 5.0),
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
        assert!(second.is_empty());
        let moved = drag(&mut gestures, &mut cam, touch(1, 0.0, 50.0), 6.0).unwrap();
        assert_ne!(cam, camera(), "the pinch moves the map");
        assert!(moved.is_empty());
        assert!(gestures.released(touch(2, 150.0, 50.0), 7.0).is_empty());
        // The remaining finger pans; it does not resume painting.
        let pinched = cam;
        let panned = drag(&mut gestures, &mut cam, touch(1, 20.0, 50.0), 8.0).unwrap();
        assert_close(cam.offset_x - pinched.offset_x, 20.0);
        assert!(panned.is_empty());
        assert!(gestures.released(touch(1, 20.0, 50.0), 9.0).is_empty());

        // A committed mouse stroke stops as soon as a pinch takes over.
        gestures.press(mouse(5.0, 5.0), Press::Stroke, 10.0);
        gestures.press(touch(9, 50.0, 50.0), Press::Pan, 11.0);
        let moved = drag(&mut gestures, &mut cam, mouse(8.0, 5.0), 12.0).unwrap();
        assert!(moved.is_empty());
    }

    #[test]
    fn cancelled_or_pinched_selections_never_commit() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        drag(&mut gestures, &mut cam, mouse(90.0, 70.0), 1.0);
        assert_eq!(
            gestures.cancelled(1, 2.0),
            [GestureEvent::SelectPreview(None)]
        );
        assert!(!gestures.is_pressed());
        // A late release for the cancelled pointer is ignored.
        assert!(gestures.released(mouse(90.0, 70.0), 3.0).is_empty());

        gestures.press(touch(1, 10.0, 10.0), Press::Select, 4.0);
        drag(&mut gestures, &mut cam, touch(1, 60.0, 60.0), 5.0);
        let pinch = gestures.press(touch(2, 200.0, 200.0), Press::Pan, 6.0);
        assert_eq!(pinch, [GestureEvent::SelectPreview(None)]);
        assert!(gestures.released(touch(1, 60.0, 60.0), 7.0).is_empty());
        assert!(gestures.released(touch(2, 200.0, 200.0), 8.0).is_empty());
    }

    #[test]
    fn selections_release_as_boxes_or_clicks() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        drag(&mut gestures, &mut cam, mouse(90.0, 70.0), 1.0);
        assert_eq!(
            gestures.released(mouse(90.0, 70.0), 2.0),
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
            gestures.released(mouse(12.0, 11.0), 4.0),
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
            gestures.released(touch(1, 31.0, 30.0), 1.0),
            [GestureEvent::Pick { x: 30.0, y: 30.0 }]
        );

        gestures.press(touch(1, 30.0, 30.0), Press::Pick, 2.0);
        gestures.press(touch(2, 90.0, 30.0), Press::Pan, 3.0);
        assert!(gestures.released(touch(1, 30.0, 30.0), 4.0).is_empty());
        assert!(gestures.cancelled(2, 5.0).is_empty());

        // A mouse pick lands on press and does not pan.
        let pressed = gestures.press(mouse(30.0, 30.0), Press::Pick, 6.0);
        assert_eq!(pressed, [GestureEvent::Pick { x: 30.0, y: 30.0 }]);
        let before = cam;
        drag(&mut gestures, &mut cam, mouse(80.0, 30.0), 7.0);
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
        drag(&mut gestures, &mut cam, touch(1, 130.0, 90.0), 3.0);
        assert_close(cam.offset_x - before.offset_x, 30.0);
        assert_close(cam.offset_y - before.offset_y, -10.0);
        assert!(gestures.released(touch(1, 130.0, 90.0), 4.0).is_empty());
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
        drag(&mut gestures, &mut cam, touch(1, 320.0, 300.0), 2.0);
        assert_close(cam.offset_x - before.offset_x, 20.0);
        assert_close(cam.scale, before.scale);
    }

    #[test]
    fn handled_presses_neither_pan_nor_tap() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        gestures.press(mouse(10.0, 10.0), Press::Handled, 0.0);
        let before = cam;
        drag(&mut gestures, &mut cam, mouse(50.0, 10.0), 1.0);
        assert_eq!(cam, before);
        assert!(gestures.released(mouse(10.0, 10.0), 2.0).is_empty());
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
        assert!(gestures.moved(mouse(10.0, 10.0), 0.0).is_none());
        assert!(!gestures.is_interacting(0.0));
    }

    #[test]
    fn teardown_drops_previews_without_committing() {
        let mut gestures = Gestures::default();
        gestures.press(mouse(10.0, 10.0), Press::Select, 0.0);
        assert_eq!(gestures.reset(), [GestureEvent::SelectPreview(None)]);
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

    const FRAME_MS: f64 = 1000.0 / 60.0;
    const REPORT_MS: f64 = 8.0;
    /// How late each report reaches the page, cycling. Recorded Firefox and Chromium drags
    /// had moves reach frames anywhere within about one report interval of being made, so
    /// some frames get two new reports and some none.
    const DELIVERY_MS: [f64; 8] = [0.5, 6.0, 2.5, 8.5, 1.0, 5.0, 3.5, 7.5];

    /// A finger dragged right at `speed` px/ms from x = 100, reporting every `REPORT_MS` with
    /// exact time stamps; the page receives each report `DELIVERY_MS` late. For each frame:
    /// where the map shows the finger, and where the newest received report put it.
    fn even_drag(speed: f64, frames: usize) -> Vec<(f64, f64)> {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let world = cam.screen_to_world(100.0, 300.0);
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        let mut next = 1;
        let mut latest_x = 100.0;
        let mut shown = Vec::new();
        for frame in 1..=frames {
            let frame_ms = frame as f64 * FRAME_MS;
            loop {
                let at_ms = next as f64 * REPORT_MS;
                if at_ms + DELIVERY_MS[next % DELIVERY_MS.len()] > frame_ms {
                    break;
                }
                latest_x = 100.0 + speed * at_ms;
                gestures.moved(touch(1, latest_x, 300.0), at_ms);
                next += 1;
            }
            gestures.frame(&mut cam, frame_ms);
            shown.push((cam.world_to_screen(world.0, world.1).0, latest_x));
        }
        shown
    }

    fn steps(values: &[f64]) -> Vec<f64> {
        values.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// How a drag looks frame by frame against the even step `expected`: frames that did not
    /// move, and frames off the even step by more than a quarter.
    fn unevenness(steps: &[f64], expected: f64) -> (usize, usize) {
        let frozen = steps
            .iter()
            .filter(|step| step.abs() < expected * 0.01)
            .count();
        let uneven = steps
            .iter()
            .filter(|step| (*step / expected - 1.0).abs() > 0.25)
            .count();
        (frozen, uneven)
    }

    #[test]
    fn a_drag_moves_on_every_frame_more_evenly_than_its_reports_and_no_later() {
        let speed = 0.4;
        let drag = even_drag(speed, 120);
        let expected = speed * FRAME_MS;
        let (shown, newest): (Vec<f64>, Vec<f64>) = drag[2..].iter().copied().unzip();
        let (raw_frozen, raw_uneven) = unevenness(&steps(&newest), expected);
        let (frozen, uneven) = unevenness(&steps(&shown), expected);
        // Showing the newest report moves the map by a step and a half on some frames and half a
        // step on others.
        assert!(raw_uneven > 10, "{raw_frozen} {raw_uneven}");
        assert_eq!(frozen, 0, "every frame of a drag moves the map");
        assert!(
            uneven * 2 < raw_uneven,
            "{uneven} uneven frames against {raw_uneven}"
        );
        // How long ago the finger was where each frame shows it, on average: no longer than
        // for the newest report.
        let lag = |x: &f64| (x - 100.0) / speed;
        let behind = |positions: &[f64]| {
            positions
                .iter()
                .enumerate()
                .map(|(frame, x)| (frame + 3) as f64 * FRAME_MS - lag(x))
                .sum::<f64>()
                / positions.len() as f64
        };
        assert!(
            behind(&shown) <= behind(&newest),
            "{} ms behind against {} ms",
            behind(&shown),
            behind(&newest)
        );
    }

    #[test]
    fn a_finger_that_stops_is_shown_where_it_stopped() {
        let speed = 1.0;
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let world = cam.screen_to_world(100.0, 300.0);
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        let shown = |cam: &Viewport| cam.world_to_screen(world.0, world.1).0;
        // Reports every 8 ms up to 96 ms, each arriving 6 ms late, then nothing.
        let stopped_x = 100.0 + speed * 96.0;
        let mut furthest: f64 = 0.0;
        let mut frame_ms = 0.0;
        let mut next = 1;
        for _ in 0..20 {
            frame_ms += FRAME_MS;
            while next <= 12 && f64::from(next) * REPORT_MS + 6.0 <= frame_ms {
                let at_ms = f64::from(next) * REPORT_MS;
                gestures.moved(touch(1, 100.0 + speed * at_ms, 300.0), at_ms);
                next += 1;
            }
            gestures.frame(&mut cam, frame_ms);
            furthest = furthest.max(shown(&cam));
        }
        assert!(
            furthest <= stopped_x + speed * MAX_PREDICTION_MS,
            "carried {} px past the stop",
            furthest - stopped_x
        );
        assert_close(shown(&cam), stopped_x);
        assert!(!gestures.frame(&mut cam, frame_ms + FRAME_MS).pending);
    }

    #[test]
    fn a_released_drag_glides_to_where_the_finger_lifted() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let world = cam.screen_to_world(100.0, 300.0);
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        let mut frame_ms = 0.0;
        for k in 1..=12 {
            let at_ms = f64::from(k) * REPORT_MS;
            gestures.moved(touch(1, 100.0 + at_ms, 300.0), at_ms);
            if at_ms - frame_ms >= FRAME_MS {
                frame_ms += FRAME_MS;
                // Each frame starts well after the reports it gets.
                gestures.frame(&mut cam, frame_ms + 12.0);
            }
        }
        let lifted = (100.0 + 12.0 * REPORT_MS + 3.0, 300.0);
        assert!(
            gestures
                .released(touch(1, lifted.0, lifted.1), 100.0)
                .is_empty()
        );
        let mut previous = cam.offset_x;
        let mut frames = 0;
        loop {
            frame_ms += FRAME_MS;
            let motion = gestures.frame(&mut cam, frame_ms);
            assert!(cam.offset_x >= previous, "the glide never backs up");
            previous = cam.offset_x;
            frames += 1;
            if !motion.pending {
                break;
            }
            assert!(frames < 10, "the glide ends");
        }
        assert_world_under(&cam, world, lifted);
        assert_eq!(
            gestures.frame(&mut cam, frame_ms + FRAME_MS),
            FrameMotion::default()
        );
    }

    #[test]
    fn a_finger_that_holds_still_stops_asking_for_frames() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let world = cam.screen_to_world(100.0, 300.0);
        gestures.press(touch(1, 100.0, 300.0), Press::Pan, 0.0);
        gestures.moved(touch(1, 130.0, 300.0), 8.0);
        gestures.moved(touch(1, 160.0, 290.0), 16.0);
        let mut frame_ms = 30.0;
        let mut motion = gestures.frame(&mut cam, frame_ms);
        let mut frames = 1;
        while motion.pending {
            frame_ms += FRAME_MS;
            motion = gestures.frame(&mut cam, frame_ms);
            frames += 1;
            assert!(frames < 5, "a still finger settles within the report delay");
        }
        assert_world_under(&cam, world, (160.0, 290.0));
        assert!(!gestures.frame(&mut cam, frame_ms + FRAME_MS).moved);
    }

    #[test]
    fn a_pinch_reported_one_finger_at_a_time_zooms_on_every_frame_as_evenly_as_its_reports() {
        let mut gestures = Gestures::default();
        let mut cam = camera();
        let (cx, cy) = (400.0, 300.0);
        // Spreading at a constant rate of zoom: four times as far apart in a second.
        let half = |at_ms: f64| 40.0 * (4.0_f64.ln() * at_ms / 1000.0).exp();
        gestures.press(touch(1, cx - half(0.0), cy), Press::Pan, 0.0);
        gestures.press(touch(2, cx + half(0.0), cy), Press::Pan, 0.0);
        // The fingers report out of step, half a report apart, and arrive late by varying
        // amounts.
        let mut reports: Vec<(f64, f64, Pointer)> = (1..200)
            .flat_map(|k| {
                let a = f64::from(k) * REPORT_MS;
                let b = a + REPORT_MS / 2.0;
                let late = |at: f64| DELIVERY_MS[(at / 4.0) as usize % DELIVERY_MS.len()];
                [
                    (a + late(a), a, touch(1, cx - half(a), cy)),
                    (b + late(b), b, touch(2, cx + half(b), cy)),
                ]
            })
            .collect();
        reports.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut reports = reports.into_iter().peekable();
        let start = cam.scale;
        let (mut newest, mut scales, mut raw) = ([half(0.0); 2], Vec::new(), Vec::new());
        for frame in 1..=60 {
            let frame_ms = f64::from(frame) * FRAME_MS;
            while let Some((_, at_ms, pointer)) = reports.next_if(|r| r.0 <= frame_ms) {
                newest[(pointer.id - 1) as usize] = (pointer.x - cx).abs();
                gestures.moved(pointer, at_ms);
            }
            gestures.frame(&mut cam, frame_ms);
            scales.push(cam.scale.ln());
            raw.push((start * (newest[0] + newest[1]) / (2.0 * half(0.0))).ln());
        }
        let expected = 4.0_f64.ln() * FRAME_MS / 1000.0;
        let (raw_frozen, raw_uneven) = unevenness(&steps(&raw[2..]), expected);
        let (frozen, uneven) = unevenness(&steps(&scales[2..]), expected);
        assert_eq!((frozen, raw_frozen), (0, 0));
        assert!(
            uneven <= raw_uneven,
            "{uneven} uneven frames against {raw_uneven}"
        );
    }
}
