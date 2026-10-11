//! Output geometry: which world area an export shows and how it maps onto pixels.

use tiny_skia::Rect;

/// World area of every export (x, z; half-open): the bundled terrain's opaque pixels plus a
/// 40-block margin. Measured from the tiles, so it changes only when they do.
const FRAME_LEFT: i32 = -2521;
const FRAME_TOP: i32 = -6619;
const FRAME_RIGHT: i32 = 1691;
const FRAME_BOTTOM: i32 = -119;

/// Tile origins and sizes are multiples of this many blocks.
const TILE_GRID: i64 = 512;

/// Empty space beside the Realm of Light, above the main continent, in world coordinates
/// (left, top, right, bottom). The title and legend live here rather than over the map.
const TITLE_AREA: [i32; 4] = [-2440, -6580, -1100, -5790];
const LEGEND_AREA: [i32; 4] = [-560, -6580, 1640, -5740];

/// Pixel geometry for one export width.
///
/// The scale is `units / 512` pixels per block, so every tile edge (a multiple of 512 blocks)
/// lands on a whole pixel and independently resampled tiles meet without seams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Frame {
    width: u32,
    height: u32,
    units: u32,
    origin_x: i64,
    origin_z: i64,
}

impl Frame {
    /// Geometry for an export exactly `width` pixels wide; the height follows from the frame.
    pub(super) fn for_width(width: u32) -> Self {
        let frame_width = i64::from(FRAME_RIGHT - FRAME_LEFT);
        let units = (i64::from(width) * TILE_GRID / frame_width).max(1);
        let left = (i64::from(FRAME_LEFT) * units).div_euclid(TILE_GRID);
        let right = ceil_div(i64::from(FRAME_RIGHT) * units, TILE_GRID);
        let top = (i64::from(FRAME_TOP) * units).div_euclid(TILE_GRID);
        let bottom = ceil_div(i64::from(FRAME_BOTTOM) * units, TILE_GRID);
        let padding = (i64::from(width) - (right - left)) / 2;
        Self {
            width,
            height: u32::try_from(bottom - top).unwrap_or(1),
            units: u32::try_from(units).unwrap_or(1),
            origin_x: left - padding,
            origin_z: top,
        }
    }

    pub(super) fn width(self) -> u32 {
        self.width
    }

    pub(super) fn height(self) -> u32 {
        self.height
    }

    /// Scale identity shared by every width that resamples terrain identically.
    pub(super) fn units(self) -> u32 {
        self.units
    }

    /// Pixels per block.
    pub(super) fn scale(self) -> f32 {
        self.units as f32 / TILE_GRID as f32
    }

    /// Exact pixel position of a block-grid-aligned world coordinate (x, z).
    pub(super) fn grid_pixel(self, x: i32, z: i32) -> (i64, i64) {
        let units = i64::from(self.units);
        (
            (i64::from(x) * units).div_euclid(TILE_GRID) - self.origin_x,
            (i64::from(z) * units).div_euclid(TILE_GRID) - self.origin_z,
        )
    }

    pub(super) fn point(self, x: f32, z: f32) -> (f32, f32) {
        let scale = self.scale();
        (
            x * scale - self.origin_x as f32,
            z * scale - self.origin_z as f32,
        )
    }

    /// Pixel rectangle covering the inclusive world bounds.
    pub(super) fn rect(self, left: i32, top: i32, right: i32, bottom: i32) -> Option<Rect> {
        let (x1, y1) = self.point(left as f32, top as f32);
        let (x2, y2) = self.point((right + 1) as f32, (bottom + 1) as f32);
        Rect::from_ltrb(x1, y1, x2, y2)
    }

    pub(super) fn title_area(self) -> Option<Rect> {
        self.area(TITLE_AREA)
    }

    pub(super) fn legend_area(self) -> Option<Rect> {
        self.area(LEGEND_AREA)
    }

    fn area(self, [left, top, right, bottom]: [i32; 4]) -> Option<Rect> {
        let (x1, y1) = self.point(left as f32, top as f32);
        let (x2, y2) = self.point(right as f32, bottom as f32);
        Rect::from_ltrb(x1, y1, x2, y2)
    }
}

fn ceil_div(value: i64, divisor: i64) -> i64 {
    -((-value).div_euclid(divisor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_requested_width_and_portrait_height() {
        let frame = Frame::for_width(2400);
        assert_eq!(frame.width(), 2400);
        assert!((3600..3800).contains(&frame.height()), "{}", frame.height());
    }

    #[test]
    fn tile_edges_land_on_whole_pixels() {
        let frame = Frame::for_width(1999);
        let (x, z) = frame.point(-1024.0, -5120.0);
        assert_eq!((x, z), {
            let (gx, gz) = frame.grid_pixel(-1024, -5120);
            (gx as f32, gz as f32)
        });
    }

    #[test]
    fn frame_content_is_centred_horizontally() {
        let frame = Frame::for_width(2400);
        let (left, _) = frame.point(FRAME_LEFT as f32, 0.0);
        let (right, _) = frame.point(FRAME_RIGHT as f32, 0.0);
        // Whole-pixel padding and block edges leave at most two pixels of asymmetry.
        assert!((left - (2400.0 - right)).abs() <= 2.0, "{left} {right}");
    }
}
