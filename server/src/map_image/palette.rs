//! Colours shared by the export's layers.

use tiny_skia::{Color, ColorU8};

pub(super) const BACKGROUND: ColorU8 = ColorU8::from_rgba(0, 0, 0, 255);
pub(super) const TEXT: ColorU8 = ColorU8::from_rgba(244, 240, 230, 255);
pub(super) const MUTED: ColorU8 = ColorU8::from_rgba(170, 177, 192, 255);
pub(super) const GOLD: ColorU8 = ColorU8::from_rgba(245, 197, 66, 255);
pub(super) const HELD: ColorU8 = ColorU8::from_rgba(255, 226, 158, 255);
pub(super) const HALO: ColorU8 = ColorU8::from_rgba(8, 10, 16, 235);
pub(super) const OUTLINE: ColorU8 = ColorU8::from_rgba(0, 0, 0, 255);
pub(super) const CONNECTION: ColorU8 = ColorU8::from_rgba(8, 10, 16, 235);
pub(super) const UNCLAIMED: ColorU8 = ColorU8::from_rgba(200, 204, 214, 150);

pub(super) fn color(value: ColorU8) -> Color {
    Color::from_rgba8(value.red(), value.green(), value.blue(), value.alpha())
}
