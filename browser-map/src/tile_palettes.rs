//! The colours of each high-quality map tile.
//!
//! Every tile is drawn in at most 256 colours, each fully opaque or fully transparent, so its
//! full-resolution pixels fit in one byte each: an index into the tile's palette. The renderer
//! keeps that index image and the palette on the GPU instead of four bytes a pixel.
//!
//! `tile_palettes.bin` holds, for each [`MAP_TILES`](sequoia_shared::MAP_TILES) entry in
//! order, a little-endian `u16` colour count (0 when the tile does not fit a palette) and 256
//! premultiplied RGBA colours, sorted as big-endian `u32`s (the key `tile_index.wgsl` searches
//! by) and padded by repeating the last one. A test checks it against the tile images; run it
//! with `SEQUOIA_WRITE_TILE_PALETTES=1` to rewrite it after the tiles change.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

/// Colours in a palette.
pub(crate) const PALETTE_LEN: usize = 256;
const ENTRY_BYTES: usize = 2 + PALETTE_LEN * 4;

static DATA: &[u8] = include_bytes!("tile_palettes.bin");

/// The palette of tile `id` (premultiplied RGBA, sorted), if it has one.
pub(crate) fn palette(id: usize) -> Option<&'static [u8]> {
    let entry = DATA.get(id * ENTRY_BYTES..(id + 1) * ENTRY_BYTES)?;
    let count = u16::from_le_bytes([entry[0], entry[1]]);
    (count > 0).then(|| &entry[2..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sequoia_shared::MAP_TILES;
    use std::path::Path;

    /// The order palettes are sorted in; the index shader searches by the same key.
    fn key(rgba: [u8; 4]) -> u32 {
        u32::from_be_bytes(rgba)
    }

    /// A tile's palette entry, built from its image: premultiplied colours as the browser
    /// decodes them, or no palette when it has partial transparency or too many colours.
    fn entry_for(rgba: &[u8]) -> Vec<u8> {
        let mut colours: Vec<[u8; 4]> = Vec::new();
        let mut fits = true;
        for pixel in rgba.chunks_exact(4) {
            let colour = match pixel[3] {
                0 => [0, 0, 0, 0],
                255 => [pixel[0], pixel[1], pixel[2], 255],
                // Premultiplying partial alpha rounds differently between browsers.
                _ => {
                    fits = false;
                    break;
                }
            };
            colours.push(colour);
        }
        colours.sort_unstable_by_key(|colour| key(*colour));
        colours.dedup();
        let mut entry = vec![0u8; ENTRY_BYTES];
        if !fits || colours.is_empty() || colours.len() > PALETTE_LEN {
            return entry;
        }
        entry[..2].copy_from_slice(&(colours.len() as u16).to_le_bytes());
        let last = *colours.last().expect("non-empty");
        for index in 0..PALETTE_LEN {
            let colour = colours.get(index).copied().unwrap_or(last);
            entry[2 + index * 4..2 + index * 4 + 4].copy_from_slice(&colour);
        }
        entry
    }

    fn decode(path: &Path) -> Vec<u8> {
        let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut decoder = image_webp::WebPDecoder::new(std::io::BufReader::new(file))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let (width, height) = decoder.dimensions();
        let channels = if decoder.has_alpha() { 4 } else { 3 };
        let mut pixels = vec![0u8; width as usize * height as usize * channels];
        decoder
            .read_image(&mut pixels)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if channels == 4 {
            return pixels;
        }
        pixels
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    #[test]
    fn palettes_match_the_tiles() {
        let tiles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../client/public/tiles");
        let expected: Vec<u8> = MAP_TILES
            .iter()
            .flat_map(|tile| entry_for(&decode(&tiles.join(tile.file))))
            .collect();
        if std::env::var_os("SEQUOIA_WRITE_TILE_PALETTES").is_some() {
            let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tile_palettes.bin");
            std::fs::write(&out, &expected).expect("write palettes");
            return;
        }
        assert!(
            DATA == expected.as_slice(),
            "tile_palettes.bin is stale: run the test with SEQUOIA_WRITE_TILE_PALETTES=1"
        );
        // Today's tiles all fit; a tile that stops fitting is drawn from four bytes a pixel.
        for (id, tile) in MAP_TILES.iter().enumerate() {
            assert!(palette(id).is_some(), "{} has no palette", tile.file);
        }
    }

    #[test]
    fn a_palette_is_sorted_and_padded() {
        let rgba = [
            9, 9, 9, 255, //
            1, 2, 3, 255, //
            7, 7, 7, 0, // transparent: premultiplied to zero
            9, 9, 9, 255,
        ];
        let entry = entry_for(&rgba);
        assert_eq!(u16::from_le_bytes([entry[0], entry[1]]), 3);
        assert_eq!(&entry[2..14], &[0, 0, 0, 0, 1, 2, 3, 255, 9, 9, 9, 255]);
        assert_eq!(&entry[entry.len() - 4..], &[9, 9, 9, 255]);
        // Partial transparency does not fit a palette.
        assert_eq!(&entry_for(&[1, 2, 3, 128])[..2], &[0, 0]);
    }
}
