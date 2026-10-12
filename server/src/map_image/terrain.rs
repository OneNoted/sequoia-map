//! Terrain base layer, resampled from the bundled one-block-per-pixel source tiles.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use sequoia_shared::{MAP_TILES, MapTile};
use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};

use super::MapImageError;
use super::assets::{premultiplied, read_webp};
use super::frame::Frame;
use super::palette;

/// Most tiles decoded and resampled at once. Each worker holds at most two tile-sized buffers
/// (under 17 MB at the default width); finished tiles are handed over one at a time.
const TERRAIN_WORKERS: usize = 4;

/// A resampled tile and the canvas position of its top-left corner.
type PlacedTile = (i32, i32, Pixmap);

/// A background-filled canvas of the frame's size with every tile drawn into it.
///
/// Tiles are decoded and resampled in parallel but drawn in table order, so the result is
/// identical to a sequential composition.
///
/// # Errors
///
/// Returns [`MapImageError`] when a tile is missing or corrupt, or the canvas cannot be
/// allocated.
pub(super) fn compose(tiles: &Path, frame: Frame) -> Result<Pixmap, MapImageError> {
    let mut canvas = Pixmap::new(frame.width(), frame.height()).ok_or(MapImageError::Canvas)?;
    canvas.fill(palette::color(palette::BACKGROUND));
    let workers = std::thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(TERRAIN_WORKERS);
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (sender, next) = (sender.clone(), &next);
            scope.spawn(move || {
                let mut resizer = Resizer::new();
                // Claim each immutable tile once; composition below preserves table order.
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(tile) = MAP_TILES.get(index) else {
                        break;
                    };
                    let result = place(&mut resizer, tiles, tile, frame);
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        // The Realm of Light overlaps the main grid's empty northern edge: keep table order.
        // A slow earlier tile can retain later results up to the fixed MAP_TILES table size;
        // the channel bounds handoff, not this reorder buffer.
        let mut waiting = Vec::new();
        let mut drawn = 0;
        for (index, result) in receiver {
            waiting.push((index, result));
            while let Some(position) = waiting.iter().position(|(index, _)| *index == drawn) {
                let (left, top, pixmap) = waiting.swap_remove(position).1?;
                canvas.draw_pixmap(
                    left,
                    top,
                    pixmap.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
                drawn += 1;
            }
        }
        Ok::<(), MapImageError>(())
    })?;
    Ok(canvas)
}

fn place(
    resizer: &mut Resizer,
    tiles: &Path,
    tile: &MapTile,
    frame: Frame,
) -> Result<PlacedTile, MapImageError> {
    let (left, top) = frame.grid_pixel(tile.x1, tile.z1);
    let (right, bottom) = frame.grid_pixel(tile.x2 + 1, tile.z2 + 1);
    let resampled = resample(resizer, tiles, tile, (right - left, bottom - top))?;
    Ok((
        i32::try_from(left).map_err(|_| MapImageError::Canvas)?,
        i32::try_from(top).map_err(|_| MapImageError::Canvas)?,
        resampled,
    ))
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
    let channels = source.channels;
    // Release the decoded tile before allocating its premultiplied copy: with several workers
    // the terrain phase otherwise holds three tile-sized buffers per worker.
    drop(source);
    let pixels = premultiplied(target.buffer(), channels);
    Pixmap::from_vec(pixels, size).ok_or(MapImageError::Canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TILES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../client/public/tiles");

    /// Mean of each colour channel over premultiplied RGBA pixels.
    fn mean_colour(rgba: &[u8]) -> [f64; 3] {
        let pixels = (rgba.len() / 4) as f64;
        std::array::from_fn(|channel| {
            rgba.chunks_exact(4)
                .map(|pixel| f64::from(pixel[channel]))
                .sum::<f64>()
                / pixels
        })
    }

    #[test]
    fn resampled_terrain_keeps_the_source_tile_colours() {
        // The website draws the bundled tiles unchanged; exports must not dim or desaturate them.
        let tiles = Path::new(TILES);
        let tile = MAP_TILES
            .iter()
            .find(|tile| tile.file == "main-2-2.webp")
            .expect("central tile");
        let source = read_webp(&tiles.join(tile.file)).expect("source tile");
        let expected = mean_colour(&premultiplied(&source.pixels, source.channels));

        let (_, _, resampled) =
            place(&mut Resizer::new(), tiles, tile, Frame::for_width(800)).expect("tile");
        let actual = mean_colour(resampled.data());

        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 2.0,
                "resampled {actual:.1}, source {expected:.1}"
            );
        }
    }

    #[test]
    fn parallel_composition_matches_drawing_in_table_order() {
        let tiles = Path::new(TILES);
        let frame = Frame::for_width(800);

        let parallel = compose(tiles, frame).expect("parallel terrain");

        let mut sequential = Pixmap::new(frame.width(), frame.height()).expect("canvas");
        sequential.fill(palette::color(palette::BACKGROUND));
        let mut resizer = Resizer::new();
        for tile in MAP_TILES {
            let (left, top, pixmap) = place(&mut resizer, tiles, tile, frame).expect("tile");
            sequential.draw_pixmap(
                left,
                top,
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        assert!(parallel.data() == sequential.data());
    }
}
