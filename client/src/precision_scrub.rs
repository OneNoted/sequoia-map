//! Precision scrubbing on the History rails: a drag that rises above its rail moves time more
//! slowly, so a pointer can settle on the minute it wants even where one pixel spans hours.
//!
//! Movement is integrated, not mapped from the press: each horizontal step moves time by the
//! rail's speed at that height, so lifting or lowering the pointer never moves time by itself and
//! a change of precision never rescales the distance already dragged. Time is kept fractional
//! until a timestamp is chosen, so steps finer than a second still add up.

/// Lift above the rail's centre that still drags at full speed, so a drag that wanders a little
/// keeps the thumb under the pointer.
pub const FULL_SPEED_LIFT_PX: f64 = 24.0;
/// Each further this many pixels of lift halves the time a horizontal pixel moves.
pub const HALVING_LIFT_PX: f64 = 48.0;
/// The slowest the rail gets: a sixty-fourth of its normal speed, six halvings up.
pub const FINEST_SCALE: f64 = 1.0 / 64.0;
/// The number of halvings between full speed and [`FINEST_SCALE`].
pub const FINEST_LEVEL: u8 = 6;

/// The share of the rail's normal speed a pointer `lift_px` above its centre drags at.
pub fn scale_for_lift(lift_px: f64) -> f64 {
    let past = (lift_px - FULL_SPEED_LIFT_PX).max(0.0);
    (-past / HALVING_LIFT_PX).exp2().max(FINEST_SCALE)
}

/// How many halvings slower than full speed `scale` is, to the nearest one.
pub fn level_for_scale(scale: f64) -> u8 {
    let halvings = (1.0 / scale).log2().round();
    halvings.clamp(0.0, f64::from(FINEST_LEVEL)) as u8
}

/// A range input as laid out now, in CSS pixels, with the times at its ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rail {
    pub left: f64,
    pub width: f64,
    /// The thumb's diameter: its centre travels from `left + thumb / 2` to `right - thumb / 2`.
    pub thumb: f64,
    pub center_y: f64,
    pub min: i64,
    pub max: i64,
}

impl Rail {
    fn travel_px(&self) -> f64 {
        (self.width - self.thumb).max(1.0)
    }

    fn secs_per_px(&self) -> f64 {
        (self.max - self.min) as f64 / self.travel_px()
    }

    fn clamp(&self, value: f64) -> f64 {
        value.clamp(self.min as f64, self.max as f64)
    }

    /// The thumb centre's x for `value`.
    pub fn x_of(&self, value: f64) -> f64 {
        let span = (self.max - self.min).max(1) as f64;
        self.left
            + self.thumb / 2.0
            + (self.clamp(value) - self.min as f64) / span * self.travel_px()
    }

    /// The time whose thumb centre is at `x`, before clamping.
    fn value_at(&self, x: f64) -> f64 {
        self.min as f64 + (x - self.left - self.thumb / 2.0) * self.secs_per_px()
    }

    fn lift(&self, y: f64) -> f64 {
        self.center_y - y
    }
}

/// One pointer holding a rail.
#[derive(Clone, Copy, Debug)]
pub struct Scrub {
    pub pointer_id: i32,
    /// Where the pointer means the thumb to be. At full speed it may run past the rail's ends,
    /// as the pointer can, so the thumb waits at an end until the pointer comes back over the
    /// rail. Finer, the thumb no longer sits under the pointer, so it holds no overshoot: a
    /// reverse drag moves time at once.
    value: f64,
    min: i64,
    max: i64,
    x: f64,
    y: f64,
    scale: f64,
}

impl Scrub {
    /// A press at (`x`, `y`) on `rail`, whose thumb shows `value`. A press within `slack` pixels
    /// of the thumb takes hold of it where it is; anywhere else the thumb jumps under the
    /// pointer, as a native range's does.
    pub fn press(pointer_id: i32, x: f64, y: f64, rail: &Rail, value: i64, slack: f64) -> Self {
        let on_thumb = (x - rail.x_of(value as f64)).abs() <= rail.thumb / 2.0 + slack;
        let value = if on_thumb {
            value as f64
        } else {
            rail.value_at(x)
        };
        Self {
            pointer_id,
            value,
            min: rail.min,
            max: rail.max,
            x,
            y,
            scale: scale_for_lift(rail.lift(y)),
        }
    }

    /// Move the pointer to (`x`, `y`) on `rail` as it is laid out now, and return the time to
    /// show. The step runs at the mean of the speeds at its two ends.
    pub fn move_to(&mut self, x: f64, y: f64, rail: &Rail) -> i64 {
        let scale = scale_for_lift(rail.lift(y));
        let step_scale = (scale_for_lift(rail.lift(self.y)) + scale) / 2.0;
        self.value += (x - self.x) * rail.secs_per_px() * step_scale;
        if step_scale < 1.0 {
            self.value = self.value.clamp(self.min as f64, self.max as f64);
        }
        self.x = x;
        self.y = y;
        self.scale = scale;
        self.timestamp()
    }

    /// The time the thumb shows: the pointer's, held within the rail's ends.
    pub fn timestamp(&self) -> i64 {
        self.value.clamp(self.min as f64, self.max as f64).round() as i64
    }

    /// The share of full speed the pointer drags at where it is now.
    pub fn scale(&self) -> f64 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6 h window on a 616 px rail with a 16 px thumb: 36 s per pixel at full speed.
    fn rail() -> Rail {
        Rail {
            left: 100.0,
            width: 616.0,
            thumb: 16.0,
            center_y: 800.0,
            min: 0,
            max: 6 * 3600,
        }
    }

    const MID: i64 = 3 * 3600;

    fn held_at_mid() -> (Scrub, f64) {
        let rail = rail();
        let x = rail.x_of(MID as f64);
        (Scrub::press(1, x, rail.center_y, &rail, MID, 4.0), x)
    }

    #[test]
    fn a_press_on_the_thumb_keeps_the_time_and_one_beside_it_jumps_there() {
        let rail = rail();
        let thumb_x = rail.x_of(MID as f64);

        let grabbed = Scrub::press(1, thumb_x + 6.0, rail.center_y, &rail, MID, 4.0);
        assert_eq!(grabbed.timestamp(), MID);

        let tapped = Scrub::press(1, thumb_x + 100.0, rail.center_y, &rail, MID, 4.0);
        assert_eq!(tapped.timestamp(), MID + 3600);
    }

    #[test]
    fn a_drag_along_the_rail_keeps_the_thumb_under_the_pointer() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        for (dx, dy) in [(40.0, -10.0), (95.0, 6.0), (-30.0, -20.0), (150.0, 0.0)] {
            let shown = scrub.move_to(x + dx, rail.center_y + dy, &rail);
            assert!(
                (rail.x_of(shown as f64) - (x + dx)).abs() < 0.05,
                "thumb left the pointer at dx {dx}"
            );
        }
    }

    #[test]
    fn the_same_stroke_lifted_moves_time_much_less_but_still_moves_it() {
        let rail = rail();
        let stroke = |lift: f64| {
            let (mut scrub, x) = held_at_mid();
            let y = rail.center_y - lift;
            scrub.move_to(x, y, &rail);
            scrub.move_to(x + 100.0, y, &rail) - MID
        };

        let near = stroke(0.0);
        let mid = stroke(FULL_SPEED_LIFT_PX + 2.0 * HALVING_LIFT_PX);
        let far = stroke(400.0);
        assert_eq!(near, 3600);
        assert_eq!(mid, 900);
        assert_eq!(far, 56);
        assert!(far > 0);
    }

    #[test]
    fn lifting_and_lowering_alone_never_moves_time() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        scrub.move_to(x + 50.0, rail.center_y, &rail);
        let before = scrub.timestamp();
        for y in (200..=800).rev().step_by(7).chain((200..=800).step_by(13)) {
            assert_eq!(scrub.move_to(x + 50.0, f64::from(y), &rail), before);
        }
    }

    #[test]
    fn changing_precision_continues_from_where_time_is_without_rescaling_the_drag() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        let coarse = scrub.move_to(x + 100.0, rail.center_y, &rail);
        assert_eq!(coarse, MID + 3600);

        // Rise to the finest precision, then drag on and come back down.
        let high = rail.center_y - 500.0;
        assert_eq!(scrub.move_to(x + 100.0, high, &rail), coarse);
        let fine = scrub.move_to(x + 164.0, high, &rail);
        assert_eq!(fine, coarse + 36);
        assert_eq!(scrub.move_to(x + 164.0, rail.center_y, &rail), fine);
        assert_eq!(scrub.move_to(x + 174.0, rail.center_y, &rail), fine + 360);
    }

    #[test]
    fn reversing_a_stroke_at_any_height_returns_to_the_same_time() {
        let rail = rail();
        for lift in [0.0, 60.0, 150.0, 400.0] {
            let (mut scrub, x) = held_at_mid();
            let y = rail.center_y - lift;
            scrub.move_to(x, y, &rail);
            let start = scrub.timestamp();
            scrub.move_to(x + 37.5, y, &rail);
            assert_eq!(scrub.move_to(x, y, &rail), start, "lift {lift}");
        }
    }

    #[test]
    fn sub_second_steps_add_up_instead_of_rounding_away() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        let y = rail.center_y - 500.0;
        scrub.move_to(x, y, &rail);
        // 0.1 px at the finest precision is 0.06 s: each alone rounds to nothing.
        for k in 1..=1000 {
            scrub.move_to(x + f64::from(k) * 0.1, y, &rail);
        }
        assert_eq!(scrub.timestamp(), MID + 56);
    }

    #[test]
    fn time_stops_at_the_rails_ends_and_waits_there_for_the_pointer() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        let end_x = rail.x_of(rail.max as f64);
        assert_eq!(scrub.move_to(end_x + 120.0, rail.center_y, &rail), rail.max);
        // Back over the overshoot: still at the end until the pointer reaches it.
        assert_eq!(scrub.move_to(end_x + 10.0, rail.center_y, &rail), rail.max);
        assert_eq!(
            scrub.move_to(end_x - 10.0, rail.center_y, &rail),
            rail.max - 360
        );
        assert_eq!(scrub.move_to(x - 2000.0, rail.center_y, &rail), rail.min);
    }

    #[test]
    fn lifting_at_an_end_forgets_the_overshoot_so_a_fine_reverse_moves_at_once() {
        let rail = rail();
        for (end, outward) in [(rail.max, 1.0), (rail.min, -1.0)] {
            let x = rail.x_of(end as f64);
            let mut scrub = Scrub::press(1, x, rail.center_y, &rail, end, 3.0);
            let past = x + outward * 12.0;
            assert_eq!(scrub.move_to(past, rail.center_y, &rail), end);

            let high = rail.center_y - 500.0;
            assert_eq!(scrub.move_to(past, high, &rail), end, "lifting moved time");
            // 60 px back at a sixty-fourth of 36 s per pixel.
            let back = scrub.move_to(past - outward * 60.0, high, &rail);
            assert_eq!(back, end - (outward * 34.0) as i64);
        }
    }

    #[test]
    fn a_resize_mid_drag_changes_speed_not_the_time_shown() {
        let rail = rail();
        let (mut scrub, x) = held_at_mid();
        let shown = scrub.move_to(x + 80.0, rail.center_y, &rail);
        let narrower = Rail {
            width: 316.0,
            ..rail
        };
        assert_eq!(scrub.move_to(x + 80.0, rail.center_y, &narrower), shown);
        assert_eq!(
            scrub.move_to(x + 90.0, rail.center_y, &narrower),
            shown + 720
        );
    }

    #[test]
    fn speed_falls_smoothly_from_full_to_the_finest() {
        assert_eq!(scale_for_lift(-50.0), 1.0);
        assert_eq!(scale_for_lift(FULL_SPEED_LIFT_PX), 1.0);
        // A small wobble past the dead zone barely changes speed.
        assert!(scale_for_lift(FULL_SPEED_LIFT_PX + 8.0) > 0.88);
        // On a phone, a sixteenth of the speed is within reach of a thumb.
        assert!(scale_for_lift(240.0) < 1.0 / 16.0);
        assert_eq!(scale_for_lift(2000.0), FINEST_SCALE);

        let mut previous = scale_for_lift(0.0);
        for lift in 1..1000 {
            let scale = scale_for_lift(f64::from(lift));
            assert!(scale <= previous);
            assert!(previous / scale < 1.02, "speed jumped at {lift} px");
            previous = scale;
        }
    }

    #[test]
    fn levels_count_halvings() {
        assert_eq!(level_for_scale(1.0), 0);
        assert_eq!(level_for_scale(0.26), 2);
        assert_eq!(level_for_scale(FINEST_SCALE), FINEST_LEVEL);
    }
}
