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

/// Scales this close (relative) count as one: a two-finger pan recomputes the scale from the
/// fingers' span, which roundoff can leave an ulp away from where it was.
const SAME_SCALE: f64 = 1e-9;
/// Offsets this close (CSS pixels) count as unmoved: a zoom held at a scale limit still
/// recomputes the offset, which roundoff can leave a hair away from where it was.
const UNMOVED_PX: f64 = 1e-6;

/// Where each frame draws the camera, given the frames before it.
///
/// While the camera pans, frames are drawn on whole device pixels
/// ([`Viewport::on_device_grid`]), so they differ by whole pixels and every layer slides by
/// the same amount. A frame that changes the scale, and a frame at rest, keeps the sub-pixel
/// phase the frame before it was drawn at. Rounding afresh while the scale eases would move
/// the whole map back and forth by up to a device pixel from one frame to the next, and
/// rounding once a zoom ends would shift it again after it stopped. The first pan after a
/// zoom puts the camera back on the grid, while the map is moving anyway.
///
/// A zoom anchored at a point on screen must anchor the camera at
/// [`Self::camera_point`] of it: the camera zooms about its own coordinates, which the
/// phase shifts, and anchoring at the screen point itself would slide the map under a
/// still cursor by the phase times the zoom's ratio less one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DrawnCamera {
    /// The last camera placed, as the gestures and host hold it, and the device pixel ratio
    /// it was placed at.
    last: Option<(Viewport, f64)>,
    /// Added to the camera's offset to draw it, in CSS pixels: at most half a device pixel.
    phase: (f64, f64),
}

impl DrawnCamera {
    /// The camera to draw `camera` at, `dpr` device pixels per CSS pixel.
    pub fn place(&mut self, camera: Viewport, dpr: f64) -> Viewport {
        let regrid = match self.last {
            None => true,
            Some((last, last_dpr)) => {
                let same_scale = (camera.scale - last.scale).abs() <= SAME_SCALE * last.scale;
                let moved = (camera.offset_x - last.offset_x).abs() > UNMOVED_PX
                    || (camera.offset_y - last.offset_y).abs() > UNMOVED_PX;
                last_dpr != dpr || (same_scale && moved)
            }
        };
        if regrid {
            let grid = camera.on_device_grid(dpr);
            self.phase = (
                grid.offset_x - camera.offset_x,
                grid.offset_y - camera.offset_y,
            );
        }
        self.last = Some((camera, dpr));
        Viewport {
            offset_x: camera.offset_x + self.phase.0,
            offset_y: camera.offset_y + self.phase.1,
            scale: camera.scale,
        }
    }

    /// The last camera placed, before its phase was added.
    pub fn camera(&self) -> Option<Viewport> {
        self.last.map(|(camera, _)| camera)
    }

    /// Where the point the last frame drew at screen position (`x`, `y`) is in the camera's
    /// own screen coordinates.
    pub fn camera_point(&self, x: f64, y: f64) -> (f64, f64) {
        (x - self.phase.0, y - self.phase.1)
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
    use super::{DrawnCamera, MAX_SCALE, MIN_SCALE, ScreenRect, Viewport};
    use crate::test_support::assert_close;

    const RATIOS: [f64; 6] = [1.0, 1.0 / 1.1, 1.25, 1.5, 2.0, 2.625];

    fn on_grid(value: f64, dpr: f64) -> bool {
        ((value * dpr) - (value * dpr).round()).abs() < 1e-6
    }

    #[test]
    fn drawn_pans_move_by_whole_device_pixels() {
        for dpr in RATIOS {
            let mut drawn = DrawnCamera::default();
            let mut camera = Viewport {
                offset_x: 120.3,
                offset_y: -48.71,
                scale: 2.75,
            };
            let mut previous = drawn.place(camera, dpr);
            for (dx, dy) in [(3.37, -0.61), (0.2, 0.2), (-5.55, 1.01), (0.0, 7.123)] {
                camera.pan(dx, dy);
                let now = drawn.place(camera, dpr);
                for (a, b, exact) in [
                    (previous.offset_x, now.offset_x, camera.offset_x),
                    (previous.offset_y, now.offset_y, camera.offset_y),
                ] {
                    assert!(on_grid(b, dpr), "{b} off the grid at {dpr}");
                    assert!(on_grid(b - a, dpr));
                    assert!((b - exact).abs() * dpr <= 0.5 + 1e-9);
                }
                previous = now;
            }
        }
    }

    #[test]
    fn drawn_zoom_keeps_the_point_under_a_still_cursor() {
        let cases: [(Viewport, &[f64]); 2] = [
            // A trackpad zoom: many small steps, in and back out.
            (
                Viewport {
                    offset_x: 333.33,
                    offset_y: -71.9,
                    scale: 0.61,
                },
                &[
                    -37.0, -21.5, -12.25, -7.0, -4.1, -2.3, -1.2, -0.6, 0.3, 9.0, 4.5,
                ],
            ),
            // Twelve notches in from far out. Anchored at the cursor itself rather than its
            // camera point, the first point drawn under it would end 1.6 px away at ratio 1.
            (
                Viewport {
                    offset_x: 100.49,
                    offset_y: 100.49,
                    scale: 0.1,
                },
                &[-120.0; 12],
            ),
        ];
        for dpr in RATIOS {
            for (start, steps) in cases {
                let mut drawn = DrawnCamera::default();
                let mut camera = start;
                let cursor = (512.4, 300.7);
                let first = drawn.place(camera, dpr);
                let world = first.screen_to_world(cursor.0, cursor.1);
                for &step in steps {
                    let (ax, ay) = drawn.camera_point(cursor.0, cursor.1);
                    camera.zoom_at(step, ax, ay);
                    let now = drawn.place(camera, dpr);
                    let (x, y) = now.world_to_screen(world.0, world.1);
                    assert!(
                        (x - cursor.0).abs() < 1e-6 && (y - cursor.1).abs() < 1e-6,
                        "{dpr}: moved to {x},{y}"
                    );
                    assert!((now.offset_x - camera.offset_x).abs() * dpr <= 0.5 + 1e-9);
                    assert!((now.offset_y - camera.offset_y).abs() * dpr <= 0.5 + 1e-9);
                }
                assert!(camera.scale > start.scale);
            }
        }
    }

    #[test]
    fn drawn_camera_holds_still_once_a_zoom_ends_and_regrids_on_the_next_pan() {
        let dpr = 1.0 / 1.1;
        let mut drawn = DrawnCamera::default();
        let mut camera = Viewport {
            offset_x: 10.0,
            offset_y: 20.0,
            scale: 1.0,
        };
        let first = drawn.place(camera, dpr);
        let phase = (
            first.offset_x - camera.offset_x,
            first.offset_y - camera.offset_y,
        );
        camera.zoom_at(-120.0, 401.3, 233.9);
        let zoomed = drawn.place(camera, dpr);
        assert_close(zoomed.offset_x - camera.offset_x, phase.0);
        assert_close(zoomed.offset_y - camera.offset_y, phase.1);
        assert!(!on_grid(zoomed.offset_x, dpr) && !on_grid(zoomed.offset_y, dpr));
        // Frames at rest (a hover, a label refresh) draw it exactly where the zoom left it.
        assert_eq!(drawn.place(camera, dpr), zoomed);
        assert_eq!(drawn.place(camera, dpr), zoomed);
        assert_eq!(drawn.camera(), Some(camera));
        camera.pan(0.4, -0.3);
        let panned = drawn.place(camera, dpr);
        assert!(on_grid(panned.offset_x, dpr) && on_grid(panned.offset_y, dpr));
    }

    #[test]
    fn drawn_zoom_held_at_the_scale_limit_does_not_move() {
        let mut drawn = DrawnCamera::default();
        let mut camera = Viewport {
            offset_x: -20.48,
            offset_y: 5.0,
            scale: 7.5,
        };
        drawn.place(camera, 1.0);
        camera.zoom_at(-100.0, 175.0, 40.0);
        assert_eq!(camera.scale, MAX_SCALE);
        let at_limit = drawn.place(camera, 1.0);
        // Further notches at the limit recompute the offset to within roundoff; the map stays
        // (rounding afresh would move it to -34, nearly a pixel).
        camera.zoom_at(-100.0, 225.0, 60.0);
        let again = drawn.place(camera, 1.0);
        assert_close(again.offset_x, at_limit.offset_x);
        assert_close(again.offset_y, at_limit.offset_y);
    }

    #[test]
    fn drawn_two_finger_pan_counts_as_a_pan() {
        let dpr = 1.0 / 1.1;
        let mut drawn = DrawnCamera::default();
        let mut camera = Viewport {
            offset_x: 12.34,
            offset_y: 56.78,
            scale: 0.61,
        };
        drawn.place(camera, dpr);
        // A pinch whose span holds still recomputes the same scale, give or take an ulp or two
        // (0.61 * 29 / 29 is 0.6100000000000001).
        camera.scale += f64::EPSILON;
        camera.pan(3.3, 1.7);
        let panned = drawn.place(camera, dpr);
        assert!(on_grid(panned.offset_x, dpr) && on_grid(panned.offset_y, dpr));
    }

    #[test]
    fn drawn_camera_regrids_when_the_pixel_ratio_changes() {
        let mut drawn = DrawnCamera::default();
        let mut camera = Viewport {
            offset_x: 10.37,
            offset_y: 20.11,
            scale: 1.0,
        };
        drawn.place(camera, 1.0);
        camera.zoom_at(-50.0, 100.0, 100.0);
        drawn.place(camera, 1.0);
        let moved = drawn.place(camera, 1.5);
        assert!(on_grid(moved.offset_x, 1.5) && on_grid(moved.offset_y, 1.5));
        // A ratio no grid can use draws the camera as it is.
        assert_eq!(DrawnCamera::default().place(camera, 0.0), camera);
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
