/// A bundled terrain tile and the world area it covers.
///
/// High-quality tiles are stored at one pixel per block under `tiles/<file>`;
/// `tiles/lq/<file>` holds half-resolution copies. Bounds are inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapTile {
    pub file: &'static str,
    pub x1: i32,
    pub z1: i32,
    pub x2: i32,
    pub z2: i32,
}

const fn tile(file: &'static str, x1: i32, z1: i32, x2: i32, z2: i32) -> MapTile {
    MapTile {
        file,
        x1,
        z1,
        x2,
        z2,
    }
}

/// Every terrain tile, in draw order: the main grid, then the Realm of Light,
/// which overlaps the grid's empty northern edge.
///
/// Coordinates from wynnmap (Zatzou/wynnmap).
pub const MAP_TILES: &[MapTile] = &[
    tile("main-1-1.webp", -2560, -6144, -1025, -5121),
    tile("main-1-2.webp", -1024, -6144, 1023, -5121),
    tile("main-1-3.webp", 1024, -6144, 2047, -5121),
    tile("main-2-1.webp", -2560, -5120, -1025, -4097),
    tile("main-2-2.webp", -1024, -5120, 1023, -4097),
    tile("main-2-3.webp", 1024, -5120, 2047, -4097),
    tile("main-3-1.webp", -2560, -4096, -1025, -3073),
    tile("main-3-2.webp", -1024, -4096, 1023, -3073),
    tile("main-3-3.webp", 1024, -4096, 2047, -3073),
    tile("main-4-1.webp", -2560, -3072, -1025, -2049),
    tile("main-4-2.webp", -1024, -3072, 1023, -2049),
    tile("main-4-3.webp", 1024, -3072, 2047, -2049),
    tile("main-5-1.webp", -2560, -2048, -1025, -1025),
    tile("main-5-2.webp", -1024, -2048, 1023, -1025),
    tile("main-5-3.webp", 1024, -2048, 2047, -1025),
    tile("main-6-1.webp", -2560, -1024, -1025, -1),
    tile("main-6-2.webp", -1024, -1024, 1023, -1),
    tile("main-6-3.webp", 1024, -1024, 2047, -1),
    tile("realm-of-light.webp", -1536, -6656, -513, -5633),
];
