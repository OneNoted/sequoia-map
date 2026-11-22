//! Which terrain tiles share one world-sized texture, and where.
//!
//! Drawing a tile costs the browser a draw call and its texture and uniform bindings; at
//! 3440x1440 in Firefox the 19 tiles' draws alone cost a sixth of the frame rate. The main
//! tiles lie side by side on a grid, so their palette indices can sit in one texture laid
//! out like the world, drawn in one call. A tile joins only if it overlaps no tile before it
//! in draw order, so the atlas never has to choose between two tiles' texels, and only on
//! whole cells of [`CELL`] blocks, so each of its mip levels up to [`CELL_LEVEL`] holds every
//! tile on whole texels, exactly as the tile's own mip chain would.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use sequoia_shared::MapTile;

/// Blocks (and full-resolution texels) along a side of an atlas cell.
pub(crate) const CELL: u32 = 512;
/// The coarsest mip level the atlas keeps: one texel per cell.
pub(crate) const CELL_LEVEL: u32 = 9;

/// The shared terrain texture's place in the world and its tiles' places in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AtlasLayout {
    /// World x and z of the atlas's first texel.
    pub(crate) origin: (i32, i32),
    /// Width and height in texels, one block each; whole cells.
    pub(crate) size: (u32, u32),
    /// For each tile, by its index in the tile table: its texel rectangle (x, y, width,
    /// height) in the atlas, or `None` when it is drawn on its own.
    pub(crate) members: Vec<Option<[u32; 4]>>,
}

impl AtlasLayout {
    /// The atlas size in cells.
    pub(crate) fn cells(&self) -> (u32, u32) {
        (self.size.0 / CELL, self.size.1 / CELL)
    }
}

/// Tile bounds are inclusive: min x, min z, max x, max z, exclusive.
fn bounds(tile: &MapTile) -> [i64; 4] {
    [
        i64::from(tile.x1.min(tile.x2)),
        i64::from(tile.z1.min(tile.z2)),
        i64::from(tile.x1.max(tile.x2)) + 1,
        i64::from(tile.z1.max(tile.z2)) + 1,
    ]
}

fn overlap(a: [i64; 4], b: [i64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The atlas for `tiles`, or `None` when fewer than two tiles could share it.
pub(crate) fn atlas_layout(tiles: &[MapTile]) -> Option<AtlasLayout> {
    let cell = i64::from(CELL);
    // In draw order, every tile that no earlier member overlaps.
    let mut members: Vec<Option<[i64; 4]>> = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let rect = bounds(tile);
        let free = members
            .iter()
            .flatten()
            .all(|member| !overlap(*member, rect));
        members.push(free.then_some(rect));
    }
    // On whole cells from the members' corner.
    let origin_x = members.iter().flatten().map(|r| r[0]).min()?;
    let origin_z = members.iter().flatten().map(|r| r[1]).min()?;
    for member in members.iter_mut() {
        if let Some(rect) = *member
            && [
                rect[0] - origin_x,
                rect[1] - origin_z,
                rect[2] - origin_x,
                rect[3] - origin_z,
            ]
            .iter()
            .any(|v| v % cell != 0)
        {
            *member = None;
        }
    }
    if members.iter().flatten().count() < 2 {
        return None;
    }
    let width = members.iter().flatten().map(|r| r[2]).max()? - origin_x;
    let height = members.iter().flatten().map(|r| r[3]).max()? - origin_z;
    let members = members
        .into_iter()
        .map(|member| {
            member.map(|r| {
                [
                    (r[0] - origin_x) as u32,
                    (r[1] - origin_z) as u32,
                    (r[2] - r[0]) as u32,
                    (r[3] - r[1]) as u32,
                ]
            })
        })
        .collect();
    Some(AtlasLayout {
        origin: (i32::try_from(origin_x).ok()?, i32::try_from(origin_z).ok()?),
        size: (u32::try_from(width).ok()?, u32::try_from(height).ok()?),
        members,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sequoia_shared::MAP_TILES;

    #[test]
    fn the_main_grid_shares_the_atlas_and_the_realm_of_light_does_not() {
        let layout = atlas_layout(MAP_TILES).expect("an atlas");
        assert_eq!(layout.origin, (-2560, -6144));
        assert_eq!(layout.size, (4608, 6144));
        assert_eq!(layout.cells(), (9, 12));
        for (tile, member) in MAP_TILES.iter().zip(&layout.members) {
            // It overlaps the grid's northern edge and is drawn over it.
            assert_eq!(
                member.is_none(),
                tile.file == "realm-of-light.webp",
                "{}",
                tile.file
            );
        }
        assert_eq!(layout.members[0], Some([0, 0, 1536, 1024]));
        assert_eq!(layout.members[4], Some([1536, 1024, 2048, 1024]));
        assert_eq!(layout.members[17], Some([3584, 5120, 1024, 1024]));
    }

    #[test]
    fn tiles_off_the_cell_grid_are_drawn_on_their_own() {
        let tile = |x1: i32, z1: i32, size: i32| MapTile {
            file: "t",
            x1,
            z1,
            x2: x1 + size - 1,
            z2: z1 + size - 1,
        };
        let layout = atlas_layout(&[tile(0, 0, 512), tile(512, 0, 512), tile(1030, 0, 512)])
            .expect("an atlas");
        assert_eq!(
            layout.members,
            vec![Some([0, 0, 512, 512]), Some([512, 0, 512, 512]), None]
        );
        assert_eq!(atlas_layout(&[tile(0, 0, 512)]), None);
    }
}
