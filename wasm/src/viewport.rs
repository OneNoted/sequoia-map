/// Viewport manages the pan/zoom transformation from world coordinates to screen coordinates.
/// Uses CSS matrix3d transforms for GPU-accelerated rendering.
#[derive(Debug, Clone)]
pub struct Viewport {
    pub offset_x: f64,
    pub offset_y: f64,
    pub scale: f64,
}

const MIN_SCALE: f64 = 0.05;
const MAX_SCALE: f64 = 8.0;
const ZOOM_SENSITIVITY: f64 = 0.001;
const FIT_SCALE_BOOST: f64 = 1.08;

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.3,
        }
    }
}

impl Viewport {
    /// Convert world coordinates to screen coordinates.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn world_to_screen(&self, wx: f64, wy: f64) -> (f64, f64) {
        (
            wx * self.scale + self.offset_x,
            wy * self.scale + self.offset_y,
        )
    }

    /// Convert screen coordinates to world coordinates.
    pub fn screen_to_world(&self, sx: f64, sy: f64) -> (f64, f64) {
        (
            (sx - self.offset_x) / self.scale,
            (sy - self.offset_y) / self.scale,
        )
    }

    /// Zoom toward a focus point (screen coordinates).
    pub fn zoom_at(&mut self, delta: f64, screen_x: f64, screen_y: f64) {
        let factor = (-delta * ZOOM_SENSITIVITY).exp();
        let new_scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = new_scale / self.scale;

        // Adjust offset so the point under the cursor stays fixed
        self.offset_x = screen_x - (screen_x - self.offset_x) * ratio;
        self.offset_y = screen_y - (screen_y - self.offset_y) * ratio;
        self.scale = new_scale;
    }

    /// Pan by screen-space delta.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.offset_x += dx;
        self.offset_y += dy;
    }

    /// Fit the viewport to show the given world-coordinate bounds with padding.
    pub fn fit_bounds(
        &mut self,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
        canvas_w: f64,
        canvas_h: f64,
    ) {
        let world_w = max_x - min_x;
        let world_h = max_y - min_y;

        if world_w <= 0.0 || world_h <= 0.0 || canvas_w <= 0.0 || canvas_h <= 0.0 {
            return;
        }

        let padding = 0.05;
        let scale_x = canvas_w / (world_w * (1.0 + padding * 2.0));
        let scale_y = canvas_h / (world_h * (1.0 + padding * 2.0));
        // Slight global zoom-in so fitted views feel less distant by default.
        self.scale = (scale_x.min(scale_y) * FIT_SCALE_BOOST).clamp(MIN_SCALE, MAX_SCALE);

        let center_x = (min_x + max_x) / 2.0;
        let center_y = (min_y + max_y) / 2.0;
        self.offset_x = canvas_w / 2.0 - center_x * self.scale;
        self.offset_y = canvas_h / 2.0 - center_y * self.scale;
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SCALE, MIN_SCALE, Viewport};

    fn assert_close(actual: f64, expected: f64) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-9,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn world_to_screen_and_back_roundtrip() {
        let vp = Viewport {
            offset_x: 120.0,
            offset_y: -48.0,
            scale: 2.75,
        };

        let samples = [(-250.0, -100.0), (0.0, 0.0), (13.5, 92.25), (800.0, -64.0)];
        for (wx, wy) in samples {
            let (sx, sy) = vp.world_to_screen(wx, wy);
            let (wx2, wy2) = vp.screen_to_world(sx, sy);
            assert_close(wx2, wx);
            assert_close(wy2, wy);
        }
    }

    #[test]
    fn zoom_at_preserves_focus_point() {
        let mut vp = Viewport {
            offset_x: 75.0,
            offset_y: -30.0,
            scale: 1.2,
        };
        let (focus_x, focus_y) = (420.0, 260.0);
        let before = vp.screen_to_world(focus_x, focus_y);

        vp.zoom_at(-250.0, focus_x, focus_y);

        let after = vp.screen_to_world(focus_x, focus_y);
        assert_close(after.0, before.0);
        assert_close(after.1, before.1);
    }

    #[test]
    fn zoom_at_clamps_to_min_max_scale() {
        let mut vp = Viewport::default();
        vp.zoom_at(1_000_000_000.0, 0.0, 0.0);
        assert_close(vp.scale, MIN_SCALE);

        vp.zoom_at(-1_000_000_000.0, 0.0, 0.0);
        assert_close(vp.scale, MAX_SCALE);
    }

    #[test]
    fn fit_bounds_centers_on_world() {
        let mut vp = Viewport::default();
        vp.fit_bounds(0.0, 0.0, 100.0, 200.0, 1000.0, 1000.0);

        let (cx, cy) = vp.world_to_screen(50.0, 100.0);
        assert_close(cx, 500.0);
        assert_close(cy, 500.0);
    }

    #[test]
    fn fit_bounds_noop_on_zero_dimensions() {
        let mut vp = Viewport {
            offset_x: 20.0,
            offset_y: 30.0,
            scale: 0.8,
        };

        vp.fit_bounds(10.0, 0.0, 10.0, 50.0, 1000.0, 1000.0);
        assert_close(vp.offset_x, 20.0);
        assert_close(vp.offset_y, 30.0);
        assert_close(vp.scale, 0.8);

        vp.fit_bounds(0.0, 0.0, 50.0, 50.0, 0.0, 1000.0);
        assert_close(vp.offset_x, 20.0);
        assert_close(vp.offset_y, 30.0);
        assert_close(vp.scale, 0.8);
    }

    #[test]
    fn pan_shifts_offset() {
        let mut vp = Viewport::default();
        vp.pan(12.5, -3.25);

        assert_close(vp.offset_x, 12.5);
        assert_close(vp.offset_y, -3.25);
    }
}
