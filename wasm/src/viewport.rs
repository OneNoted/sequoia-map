use crate::gesture::ScreenRect;

/// The map camera: the pan/zoom transform from world coordinates to canvas CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub offset_x: f64,
    pub offset_y: f64,
    pub scale: f64,
}

pub const MIN_SCALE: f64 = 0.05;
pub const MAX_SCALE: f64 = 8.0;
const ZOOM_SENSITIVITY: f64 = 0.001;
const FIT_SCALE_BOOST: f64 = 1.08;

/// A world-space rectangle, `left`/`top` its least coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl WorldRect {
    pub fn contains(&self, other: &WorldRect) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }

    pub fn overlaps(&self, other: &WorldRect) -> bool {
        self.left <= other.right
            && other.left <= self.right
            && self.top <= other.bottom
            && other.top <= self.bottom
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }

    /// Grown by `x` on the left and right and by `y` on the top and bottom.
    pub fn grown(&self, x: f64, y: f64) -> Self {
        Self {
            left: self.left - x,
            top: self.top - y,
            right: self.right + x,
            bottom: self.bottom + y,
        }
    }
}

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

    /// The world area a `width` x `height` canvas shows.
    pub fn visible(&self, width: f64, height: f64) -> WorldRect {
        let (left, top) = self.screen_to_world(0.0, 0.0);
        let (right, bottom) = self.screen_to_world(width, height);
        WorldRect {
            left: left.min(right),
            top: top.min(bottom),
            right: left.max(right),
            bottom: top.max(bottom),
        }
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

    /// This camera with its translation moved by under half a device pixel onto a whole number
    /// of them, at `dpr` device pixels per CSS pixel. Frames drawn this way differ, while the
    /// scale holds, by whole device pixels, so every layer slides by the same pixels instead
    /// of each being re-filtered or re-snapped at a fraction of one.
    pub fn on_device_grid(&self, dpr: f64) -> Self {
        if !(dpr.is_finite() && dpr > 0.0) {
            return *self;
        }
        Self {
            offset_x: (self.offset_x * dpr).round() / dpr,
            offset_y: (self.offset_y * dpr).round() / dpr,
            scale: self.scale,
        }
    }

    /// Place the world point `world` under the screen point `screen` at `scale`, clamped to the
    /// zoom limits. The anchor stays put even when the requested scale is out of range.
    pub fn anchor(&mut self, world: (f64, f64), screen: (f64, f64), scale: f64) {
        self.scale = scale.clamp(MIN_SCALE, MAX_SCALE);
        self.offset_x = screen.0 - world.0 * self.scale;
        self.offset_y = screen.1 - world.1 * self.scale;
    }

    /// Centre the world point `world` in a `width` x `height` canvas, keeping the scale.
    pub fn center_on(&mut self, world: (f64, f64), width: f64, height: f64) {
        self.offset_x = width * 0.5 - world.0 * self.scale;
        self.offset_y = height * 0.5 - world.1 * self.scale;
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
        self.fit_bounds_within(
            min_x,
            min_y,
            max_x,
            max_y,
            ScreenRect::spanning((0.0, 0.0), (canvas_w, canvas_h)),
        );
    }

    /// [`Self::fit_bounds`] into `area` of the canvas, such as the part no panel covers.
    pub fn fit_bounds_within(
        &mut self,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
        area: ScreenRect,
    ) {
        let world_w = max_x - min_x;
        let world_h = max_y - min_y;
        let (area_w, area_h) = (area.width(), area.height());

        if world_w <= 0.0 || world_h <= 0.0 || area_w <= 0.0 || area_h <= 0.0 {
            return;
        }

        let padding = 0.05;
        let scale_x = area_w / (world_w * (1.0 + padding * 2.0));
        let scale_y = area_h / (world_h * (1.0 + padding * 2.0));
        // Slight global zoom-in so fitted views feel less distant by default.
        let scale = (scale_x.min(scale_y) * FIT_SCALE_BOOST).clamp(MIN_SCALE, MAX_SCALE);
        let center = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let area_center = (
            (area.left + area.right) / 2.0,
            (area.top + area.bottom) / 2.0,
        );
        self.anchor(center, area_center, scale);
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SCALE, MIN_SCALE, ScreenRect, Viewport};
    use crate::test_support::assert_close;

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
    fn on_device_grid_moves_pans_by_whole_device_pixels() {
        let rest = Viewport {
            offset_x: 120.3,
            offset_y: -48.71,
            scale: 2.75,
        };
        for dpr in [1.0, 1.25, 1.5, 2.0, 2.625] {
            let grid = rest.on_device_grid(dpr);
            assert_eq!(grid.scale, rest.scale);
            for (offset, snapped) in [
                (rest.offset_x, grid.offset_x),
                (rest.offset_y, grid.offset_y),
            ] {
                assert!((snapped - offset).abs() * dpr <= 0.5 + 1e-9);
                assert_close(snapped * dpr, (snapped * dpr).round());
            }
            // A pan by any fraction of a pixel lands within half a device pixel of it, on the grid.
            let mut panned = rest;
            panned.pan(3.37, -0.61);
            let moved = panned.on_device_grid(dpr);
            let dx = (moved.offset_x - grid.offset_x) * dpr;
            assert_close(dx, dx.round());
            assert!((moved.offset_x - panned.offset_x).abs() * dpr <= 0.5 + 1e-9);
        }
        assert_eq!(rest.on_device_grid(0.0), rest);
        assert_eq!(rest.on_device_grid(f64::NAN), rest);
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
    fn fit_bounds_within_centres_in_the_area_and_fits_its_size() {
        let area = ScreenRect {
            left: 0.0,
            top: 60.0,
            right: 620.0,
            bottom: 1000.0,
        };
        let mut vp = Viewport::default();
        vp.fit_bounds_within(-100.0, -100.0, 100.0, 100.0, area);

        let (cx, cy) = vp.world_to_screen(0.0, 0.0);
        assert_close(cx, 310.0);
        assert_close(cy, 530.0);
        let (left, top) = vp.world_to_screen(-100.0, -100.0);
        let (right, bottom) = vp.world_to_screen(100.0, 100.0);
        assert!(left >= area.left && right <= area.right, "{left}..{right}");
        assert!(top >= area.top && bottom <= area.bottom, "{top}..{bottom}");

        let mut whole = Viewport::default();
        whole.fit_bounds(-100.0, -100.0, 100.0, 100.0, 620.0, 940.0);
        assert_close(vp.scale, whole.scale);
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
    fn anchor_keeps_the_world_point_under_the_screen_point_at_the_limits() {
        let mut vp = Viewport::default();
        vp.anchor((120.0, -40.0), (300.0, 200.0), 1_000.0);
        assert_close(vp.scale, MAX_SCALE);
        let (sx, sy) = vp.world_to_screen(120.0, -40.0);
        assert_close(sx, 300.0);
        assert_close(sy, 200.0);
    }
}
