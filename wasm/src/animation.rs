#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use sequoia_shared::colors::{hsl_to_rgb, interpolate_hsl, rgb_to_hsl};

/// A color transition animation for territory ownership changes.
#[derive(Debug, Clone)]
pub struct ColorTransition {
    pub from_hsl: (f64, f64, f64),
    pub to_hsl: (f64, f64, f64),
    pub start_time: f64,
    pub duration: f64, // milliseconds
}

impl ColorTransition {
    pub fn new(from: (u8, u8, u8), to: (u8, u8, u8), start_time: f64, duration: f64) -> Self {
        Self {
            from_hsl: rgb_to_hsl(from.0, from.1, from.2),
            to_hsl: rgb_to_hsl(to.0, to.1, to.2),
            start_time,
            duration,
        }
    }

    /// Returns the current interpolated color as (r, g, b), or None if animation is complete.
    pub fn current_color(&self, now: f64) -> Option<(u8, u8, u8)> {
        let elapsed = now - self.start_time;
        if elapsed >= self.duration {
            return None;
        }

        let t = cubic_ease_out(elapsed / self.duration);
        let hsl = interpolate_hsl(self.from_hsl, self.to_hsl, t);
        Some(hsl_to_rgb(hsl.0, hsl.1, hsl.2))
    }

    /// Returns the flash intensity (0.0..1.0) for the initial flash overlay.
    /// Flash duration scales with transition duration: 25% of duration, capped at 200ms.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn flash_intensity(&self, now: f64) -> f64 {
        let flash_duration = (self.duration * 0.25).min(200.0);
        let elapsed = now - self.start_time;
        if elapsed >= flash_duration {
            return 0.0;
        }
        let t = elapsed / flash_duration;
        (1.0 - t) * 0.6
    }
}

/// Cubic ease-out: decelerating to zero velocity.
fn cubic_ease_out(t: f64) -> f64 {
    let t = t - 1.0;
    t * t * t + 1.0
}

#[cfg(test)]
mod tests {
    use super::{ColorTransition, cubic_ease_out};

    fn assert_close(actual: f64, expected: f64) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-9,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn cubic_ease_out_boundaries() {
        assert_close(cubic_ease_out(0.0), 0.0);
        assert_close(cubic_ease_out(1.0), 1.0);
    }

    #[test]
    fn current_color_none_when_past_duration() {
        let transition = ColorTransition::new((255, 0, 0), (0, 0, 255), 1000.0, 500.0);
        assert_eq!(transition.current_color(1500.0), None);
        assert_eq!(transition.current_color(1501.0), None);
    }

    #[test]
    fn current_color_some_during_animation() {
        let transition = ColorTransition::new((255, 0, 0), (0, 0, 255), 1000.0, 500.0);
        assert!(transition.current_color(1200.0).is_some());
    }

    #[test]
    fn flash_intensity_decays_to_zero() {
        let transition = ColorTransition::new((255, 0, 0), (0, 255, 0), 0.0, 400.0);
        assert_close(transition.flash_intensity(0.0), 0.6);
        assert_close(transition.flash_intensity(50.0), 0.3);
        assert_close(transition.flash_intensity(100.0), 0.0);
    }

    #[test]
    fn flash_intensity_caps_flash_duration() {
        let transition = ColorTransition::new((255, 0, 0), (0, 255, 0), 0.0, 2000.0);
        assert!(transition.flash_intensity(150.0) > 0.0);
        assert_close(transition.flash_intensity(200.0), 0.0);
        assert_close(transition.flash_intensity(300.0), 0.0);
    }
}
