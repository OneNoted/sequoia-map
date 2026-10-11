//! Output codecs for a finished, fully opaque canvas.

use std::io::Write;

use tiny_skia::Pixmap;

use super::{ImageFormat, MapImageError};

/// Lossy WebP quality: labels and borders stay sharp while terrain texture compresses well.
const WEBP_QUALITY: f32 = 90.0;
/// libwebp effort, 0 (fast) to 6 (smallest).
const WEBP_METHOD: i32 = 4;

/// Encodes `canvas` in `format`.
///
/// # Errors
///
/// Returns [`MapImageError::Encode`] when the codec rejects the image.
pub(super) fn encode(canvas: &Pixmap, format: ImageFormat) -> Result<Vec<u8>, MapImageError> {
    match format {
        ImageFormat::Png => png(canvas),
        ImageFormat::Webp => webp(canvas),
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

fn webp(canvas: &Pixmap) -> Result<Vec<u8>, MapImageError> {
    let mut config = webp::WebPConfig::new()
        .map_err(|()| MapImageError::Encode("libwebp rejected its default config".to_string()))?;
    config.quality = WEBP_QUALITY;
    config.method = WEBP_METHOD;
    // Measured at 4096 pixels wide: libwebp's sharp YUV conversion and token buffer add about
    // 160 MB and 100 MB of working memory, for no visible gain on these labels and borders.
    config.use_sharp_yuv = 0;
    config.low_memory = 1;
    let encoded = webp::Encoder::from_rgba(canvas.data(), canvas.width(), canvas.height())
        .encode_advanced(&config)
        .map_err(|error| MapImageError::Encode(format!("libwebp: {error:?}")))?;
    Ok(encoded.to_vec())
}
