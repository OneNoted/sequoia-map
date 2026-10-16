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

/// Keeps dark guild hues readable as filled lettering over a consistent black outline.
pub(super) fn label_color(rgb: [u8; 3]) -> Color {
    // Relative luminance: https://www.w3.org/WAI/WCAG22/Techniques/general/G18
    let [r, g, b] = rgb.map(|channel| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let [r, g, b] = if luminance >= 0.18 {
        rgb
    } else {
        rgb.map(|channel| ((u16::from(channel) + 255).div_ceil(2)) as u8)
    };
    Color::from_rgba8(r, g, b, 255)
}
