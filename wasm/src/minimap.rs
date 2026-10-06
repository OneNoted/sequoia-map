//! Placement and projection of the overview minimap in the map canvas.
//!
//! One [`MinimapLayout`] serves drawing, pointer hit-testing and the terrain cache, so the
//! minimap a user clicks is exactly the one on screen.

/// Minimap size and left margin, in CSS pixels.
pub const MINIMAP_WIDTH: f32 = 200.0;
pub const MINIMAP_HEIGHT: f32 = 280.0;
pub const MINIMAP_MARGIN: f32 = 16.0;
/// World region shown until tiles or territories have loaded: (min x, min z, max x, max z).
pub const DEFAULT_WORLD_BOUNDS: (f64, f64, f64, f64) = (-2200.0, -6600.0, 1600.0, 400.0);

/// Where the minimap is and how it maps the world, for one canvas size.
///
/// Two equal layouts rasterise identically, so the terrain cache is keyed on the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapLayout {
    /// Top-left corner in canvas CSS pixels.
    pub x: f32,
    pub y: f32,
    /// The world region letterboxed into the minimap: (min x, min z, max x, max z).
    pub world: (f64, f64, f64, f64),
    /// Projection into canvas CSS pixels: `screen = world * scale + offset`.
    pub scale: f32,
    pub offset: [f32; 2],
    /// The minimap rectangle in physical pixels (x, y, width, height), clipped to the canvas.
    pub scissor: [u32; 4],
    pub device_pixel_ratio: f32,
}

impl MinimapLayout {
    /// Lays the minimap out in the bottom-left corner of a `width` x `height` (physical
    /// pixels) canvas, `bottom_inset` CSS pixels above its bottom edge, showing `world`.
    /// `None` when there is no room for it.
    pub fn new(
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
        bottom_inset: f32,
        world: Option<(f64, f64, f64, f64)>,
    ) -> Option<Self> {
        let dpr = device_pixel_ratio;
        let css_height = height as f32 / dpr;
        let x = MINIMAP_MARGIN;
        let y = (css_height - MINIMAP_HEIGHT - bottom_inset).max(0.0);
        let scissor_x = (x * dpr).floor().max(0.0) as u32;
        let scissor_y = (y * dpr).floor().max(0.0) as u32;
        let scissor_w = ((MINIMAP_WIDTH * dpr).ceil() as u32).min(width.saturating_sub(scissor_x));
        let scissor_h =
            ((MINIMAP_HEIGHT * dpr).ceil() as u32).min(height.saturating_sub(scissor_y));
        if scissor_w == 0 || scissor_h == 0 {
            return None;
        }

        let world = world.unwrap_or(DEFAULT_WORLD_BOUNDS);
        let (min_x, min_z, max_x, max_z) = world;
        let world_w = (max_x - min_x).max(1.0) as f32;
        let world_h = (max_z - min_z).max(1.0) as f32;
        let scale = (MINIMAP_WIDTH / world_w).min(MINIMAP_HEIGHT / world_h);
        let used_w = world_w * scale;
        let used_h = world_h * scale;
        Some(Self {
            x,
            y,
            world,
            scale,
            offset: [
                x + (MINIMAP_WIDTH - used_w) * 0.5 - (min_x as f32) * scale,
                y + (MINIMAP_HEIGHT - used_h) * 0.5 - (min_z as f32) * scale,
            ],
            scissor: [scissor_x, scissor_y, scissor_w, scissor_h],
            device_pixel_ratio: dpr,
        })
    }

    /// Whether a canvas point (CSS pixels) is on the minimap.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (left, top) = (f64::from(self.x), f64::from(self.y));
        x >= left
            && x <= left + f64::from(MINIMAP_WIDTH)
            && y >= top
            && y <= top + f64::from(MINIMAP_HEIGHT)
    }

    /// The world point drawn at a canvas point, clamped to the region shown.
    pub fn screen_to_world(&self, x: f64, y: f64) -> (f64, f64) {
        let scale = f64::from(self.scale);
        let (min_x, min_z, max_x, max_z) = self.world;
        (
            ((x - f64::from(self.offset[0])) / scale).clamp(min_x, max_x),
            ((y - f64::from(self.offset[1])) / scale).clamp(min_z, max_z),
        )
    }

    /// Where a world point is drawn, in canvas CSS pixels.
    pub fn world_to_screen(&self, x: f64, z: f64) -> (f64, f64) {
        let scale = f64::from(self.scale);
        (
            x * scale + f64::from(self.offset[0]),
            z * scale + f64::from(self.offset[1]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: (f64, f64, f64, f64) = (-2560.0, -6656.0, 2048.0, 0.0);

    #[test]
    fn sits_in_the_bottom_left_corner_above_the_inset() {
        let layout = MinimapLayout::new(2400, 1600, 2.0, 16.0, Some(WORLD)).unwrap();
        assert_eq!((layout.x, layout.y), (16.0, 800.0 - 280.0 - 16.0));
        assert_eq!(layout.scissor, [32, 1008, 400, 560]);

        let raised = MinimapLayout::new(2400, 1600, 2.0, 68.0, Some(WORLD)).unwrap();
        assert_eq!(raised.y, 800.0 - 280.0 - 68.0);
        assert_ne!(raised, layout);
    }

    #[test]
    fn letterboxes_the_world_and_centres_it() {
        let layout = MinimapLayout::new(1200, 800, 1.0, 16.0, Some(WORLD)).unwrap();
        let (left, top) = layout.world_to_screen(WORLD.0, WORLD.1);
        let (right, bottom) = layout.world_to_screen(WORLD.2, WORLD.3);
        // Taller than wide: full height, centred horizontally.
        assert!((top - f64::from(layout.y)).abs() < 1e-3);
        assert!((bottom - f64::from(layout.y + MINIMAP_HEIGHT)).abs() < 1e-3);
        let margin_left = left - f64::from(layout.x);
        let margin_right = f64::from(layout.x + MINIMAP_WIDTH) - right;
        assert!(margin_left > 0.0 && (margin_left - margin_right).abs() < 1e-3);
    }

    #[test]
    fn hit_testing_inverts_the_drawn_projection() {
        let layout = MinimapLayout::new(1200, 800, 1.0, 16.0, Some(WORLD)).unwrap();
        let (sx, sy) = layout.world_to_screen(-1200.0, -3100.0);
        assert!(layout.contains(sx, sy));
        let (wx, wz) = layout.screen_to_world(sx, sy);
        assert!((wx + 1200.0).abs() < 0.05 && (wz + 3100.0).abs() < 0.05);

        // The letterbox margin maps to the nearest world edge.
        let (edge_x, _) = layout.screen_to_world(f64::from(layout.x) + 0.5, sy);
        assert_eq!(edge_x, WORLD.0);
        assert!(!layout.contains(f64::from(layout.x) - 1.0, sy));
    }

    #[test]
    fn falls_back_to_default_bounds_and_vanishes_without_room() {
        let layout = MinimapLayout::new(1200, 800, 1.0, 16.0, None).unwrap();
        assert_eq!(layout.world, DEFAULT_WORLD_BOUNDS);
        assert!(MinimapLayout::new(10, 800, 1.0, 16.0, None).is_none());
    }
}
