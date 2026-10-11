//! Fonts, icons and tiles the renderer reads from the website's static assets.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image_webp::WebPDecoder;
use tiny_skia::{IntSize, Pixmap};

use super::glyphs::Face;

/// A bundled file that is missing or unreadable; a deployment problem rather than a request one.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("cannot read map asset {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot decode map asset {path}: {source}")]
    Decode {
        path: PathBuf,
        source: image_webp::DecodingError,
    },
    #[error("cannot use map asset {path}: {reason}")]
    Invalid { path: PathBuf, reason: &'static str },
}

/// Fonts and icons drawn on every export, plus where the terrain tiles live.
pub(super) struct Assets {
    pub(super) bold: Face,
    pub(super) regular: Face,
    /// The website's 32-pixel headquarters crown, drawn two source pixels per art pixel.
    pub(super) crown: Pixmap,
    pub(super) tiles: PathBuf,
}

impl Assets {
    /// Loads fonts and icons from a website asset directory (`client/dist` in production).
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] naming the first file that is missing or unusable.
    pub(super) fn load(root: &Path) -> Result<Self, AssetError> {
        Ok(Self {
            bold: font(&root.join("fonts/minecraft-bold.otf"))?,
            regular: font(&root.join("fonts/minecraft-regular.otf"))?,
            crown: icon(&root.join("icons/crown_icon.webp"))?,
            tiles: root.join("tiles"),
        })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, AssetError> {
    std::fs::read(path).map_err(|source| AssetError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn font(path: &Path) -> Result<Face, AssetError> {
    Face::parse(&read(path)?).map_err(|_| AssetError::Invalid {
        path: path.to_path_buf(),
        reason: "not a font with the required glyphs",
    })
}

fn icon(path: &Path) -> Result<Pixmap, AssetError> {
    let image = read_webp(path)?;
    let invalid = || AssetError::Invalid {
        path: path.to_path_buf(),
        reason: "icon is empty",
    };
    let size = IntSize::from_wh(image.width, image.height).ok_or_else(invalid)?;
    Pixmap::from_vec(
        premultiplied(&image.pixels, image.channels, |value| value),
        size,
    )
    .ok_or_else(invalid)
}

/// Decoded straight-alpha WebP pixels: RGB, or RGBA when the file has alpha.
pub(super) struct DecodedImage {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) channels: usize,
    pub(super) pixels: Vec<u8>,
}

pub(super) fn read_webp(path: &Path) -> Result<DecodedImage, AssetError> {
    let decode_error = |source| AssetError::Decode {
        path: path.to_path_buf(),
        source,
    };
    let mut decoder = WebPDecoder::new(Cursor::new(read(path)?)).map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let mut pixels = vec![0; decoder.output_buffer_size().unwrap_or(0)];
    decoder.read_image(&mut pixels).map_err(decode_error)?;
    Ok(DecodedImage {
        width,
        height,
        channels,
        pixels,
    })
}

/// Converts straight RGB(A) to premultiplied RGBA, passing each colour through `grade`.
pub(super) fn premultiplied(
    pixels: &[u8],
    channels: usize,
    grade: impl Fn([f32; 3]) -> [f32; 3],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() / channels * 4);
    for pixel in pixels.chunks_exact(channels) {
        let alpha = pixel.get(3).copied().unwrap_or(u8::MAX);
        let coverage = f32::from(alpha) / 255.0;
        let graded = grade([pixel[0], pixel[1], pixel[2]].map(f32::from));
        out.extend(graded.map(|channel| (channel * coverage).round().clamp(0.0, 255.0) as u8));
        out.push(alpha);
    }
    out
}
