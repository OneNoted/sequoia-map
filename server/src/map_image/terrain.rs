//! Terrain base layer, resampled from the bundled one-block-per-pixel source tiles.

use std::path::Path;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use sequoia_shared::{MAP_TILES, MapTile};
use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};

use super::MapImageError;
use super::assets::{premultiplied, read_webp};
use super::frame::Frame;
use super::palette;

/// Terrain stays recognisable but recedes slightly, so guild colour and labels lead.
const SATURATION: f32 = 0.72;
const BRIGHTNESS: f32 = 0.84;

/// A background-filled canvas of the frame's size with every tile drawn into it.
///
/// # Errors
///
/// Returns [`MapImageError`] when a tile is missing or corrupt, or the canvas cannot be
/// allocated.
pub(super) fn compose(tiles: &Path, frame: Frame) -> Result<Pixmap, MapImageError> {
    let mut canvas = Pixmap::new(frame.width(), frame.height()).ok_or(MapImageError::Canvas)?;
    canvas.fill(palette::color(palette::BACKGROUND));
    let mut resizer = Resizer::new();
    for tile in MAP_TILES {
        let (left, top) = frame.grid_pixel(tile.x1, tile.z1);
        let (right, bottom) = frame.grid_pixel(tile.x2 + 1, tile.z2 + 1);
        let resampled = resample(&mut resizer, tiles, tile, (right - left, bottom - top))?;
        canvas.draw_pixmap(
            i32::try_from(left).map_err(|_| MapImageError::Canvas)?,
            i32::try_from(top).map_err(|_| MapImageError::Canvas)?,
            resampled.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    Ok(canvas)
}

fn resample(
    resizer: &mut Resizer,
    tiles: &Path,
    tile: &MapTile,
    (width, height): (i64, i64),
) -> Result<Pixmap, MapImageError> {
    let size = u32::try_from(width)
        .ok()
        .zip(u32::try_from(height).ok())
        .and_then(|(width, height)| IntSize::from_wh(width, height))
        .ok_or(MapImageError::Canvas)?;
    let source = read_webp(&tiles.join(tile.file))?;
    let pixel_type = if source.channels == 4 {
        PixelType::U8x4
    } else {
        PixelType::U8x3
    };
    let resample_error = |error: &dyn std::fmt::Display| MapImageError::Resample {
        file: tile.file,
        message: error.to_string(),
    };
    let view = ImageRef::new(source.width, source.height, &source.pixels, pixel_type)
        .map_err(|error| resample_error(&error))?;
    let mut target = Image::new(size.width(), size.height(), pixel_type);
    resizer
        .resize(
            &view,
            &mut target,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)),
        )
        .map_err(|error| resample_error(&error))?;
    let pixels = premultiplied(target.buffer(), source.channels, grade);
    Pixmap::from_vec(pixels, size).ok_or(MapImageError::Canvas)
}

fn grade(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    rgb.map(|channel| (luma + (channel - luma) * SATURATION) * BRIGHTNESS)
}
