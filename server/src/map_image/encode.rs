//! Output codecs for a finished, fully opaque canvas.

use std::io::Write;

use tiny_skia::Pixmap;

use super::{ImageFormat, MapImageError};

/// Lossy WebP quality: labels and borders stay sharp while terrain texture compresses well.
const WEBP_QUALITY: f32 = 90.0;
/// libwebp effort, 0 (fast) to 6 (smallest).
const WEBP_METHOD: i32 = 4;
/// Largest canvas encoded with libwebp's sharp YUV conversion, in pixels.
///
/// Plain 4:2:0 conversion averages chroma over 2x2 blocks, which pales thin saturated guild
/// borders and legend swatches: at 2657x4089 (the Discord attachment) their chroma measured
/// 0.81 of the lossless export plain and 0.93 sharp. Sharp YUV adds about 6 bytes of working
/// memory per pixel, so larger exports, including the approved 4096-pixel default, keep plain
/// conversion.
const SHARP_YUV_MAX_PIXELS: u64 = 12_000_000;

/// Encodes `canvas` in `format`.
///
/// # Errors
///
/// Returns [`MapImageError::Encode`] when the codec rejects the image.
pub(super) fn encode(canvas: &Pixmap, format: ImageFormat) -> Result<Vec<u8>, MapImageError> {
    match format {
        ImageFormat::Png => png(canvas),
        ImageFormat::Webp => {
            let pixels = u64::from(canvas.width()) * u64::from(canvas.height());
            webp(canvas, pixels <= SHARP_YUV_MAX_PIXELS)
        }
    }
}

fn png(canvas: &Pixmap) -> Result<Vec<u8>, MapImageError> {
    let encode_error = |error: &dyn std::fmt::Display| MapImageError::Encode(error.to_string());
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, canvas.width(), canvas.height());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Balanced);
    let mut writer = encoder
        .write_header()
        .map_err(|error| encode_error(&error))?;
    let mut stream = writer
        .stream_writer()
        .map_err(|error| encode_error(&error))?;
    // The canvas starts from an opaque background, so its premultiplied RGB is the straight
    // colour. Converting a row at a time avoids a second full-size buffer.
    let mut row = Vec::with_capacity(canvas.width() as usize * 3);
    for pixels in canvas.data().chunks_exact(canvas.width() as usize * 4) {
        row.clear();
        row.extend(
            pixels
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
        );
        stream
            .write_all(&row)
            .map_err(|error| encode_error(&error))?;
    }
    stream.finish().map_err(|error| encode_error(&error))?;
    writer.finish().map_err(|error| encode_error(&error))?;
    Ok(out)
}

fn webp(canvas: &Pixmap, sharp_yuv: bool) -> Result<Vec<u8>, MapImageError> {
    let mut config = webp::WebPConfig::new()
        .map_err(|()| MapImageError::Encode("libwebp rejected its default config".to_string()))?;
    config.quality = WEBP_QUALITY;
    config.method = WEBP_METHOD;
    config.use_sharp_yuv = i32::from(sharp_yuv);
    // The token buffer adds about 100 MB at 4096 pixels wide and does not change the output.
    config.low_memory = 1;
    let encoded = webp::Encoder::from_rgba(canvas.data(), canvas.width(), canvas.height())
        .encode_advanced(&config)
        .map_err(|error| MapImageError::Encode(format!("libwebp: {error:?}")))?;
    Ok(encoded.to_vec())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tiny_skia::{Color, Paint, Rect, Transform};

    use super::*;

    /// Mean of max(r, g, b) - min(r, g, b) over the pixels where `mask` holds.
    fn chroma(rgb: &[u8], mask: impl Fn(usize) -> bool) -> f64 {
        let (sum, count) = rgb
            .chunks_exact(3)
            .enumerate()
            .filter(|(index, _)| mask(*index))
            .fold((0.0, 0.0), |(sum, count), (_, pixel)| {
                let high = f64::from(*pixel.iter().max().unwrap_or(&0));
                let low = f64::from(*pixel.iter().min().unwrap_or(&0));
                (sum + high - low, count + 1.0)
            });
        sum / count
    }

    #[test]
    fn thin_saturated_borders_keep_their_colour_through_webp() {
        // Two-pixel guild-blue borders over muted terrain, as on the Discord-size export.
        let size = 512;
        let mut canvas = Pixmap::new(size, size).expect("canvas");
        canvas.fill(Color::from_rgba8(70, 92, 52, 255));
        let mut blue = Paint::default();
        blue.set_color(Color::from_rgba8(16, 16, 254, 255));
        for offset in (8..size).step_by(16) {
            let line = Rect::from_xywh(offset as f32, 0.0, 2.0, size as f32).expect("line");
            canvas.fill_rect(line, &blue, Transform::identity(), None);
        }
        let on_border =
            |index: usize| (index as u32 % size) % 16 >= 8 && (index as u32 % size) % 16 < 10;
        let source: Vec<u8> = canvas
            .data()
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();

        let retained = |sharp_yuv: bool| {
            let encoded = webp(&canvas, sharp_yuv).expect("webp");
            let mut decoder = image_webp::WebPDecoder::new(Cursor::new(encoded)).expect("decoder");
            let mut decoded = vec![0; decoder.output_buffer_size().expect("size")];
            decoder.read_image(&mut decoded).expect("decode");
            chroma(&decoded, on_border) / chroma(&source, on_border)
        };
        let (plain, sharp) = (retained(false), retained(true));

        // Measured 0.717 plain and 0.836 sharp on this canvas.
        assert!(
            sharp >= 0.8 && sharp - plain >= 0.08,
            "plain {plain:.3}, sharp {sharp:.3}"
        );
        let chosen = encode(&canvas, ImageFormat::Webp).expect("webp");
        assert!(chosen == webp(&canvas, true).expect("sharp webp"));
    }
}
