use crate::territory::ClientTerritoryMap;

const GRID_COLS: usize = 50;
const GRID_ROWS: usize = 50;

/// A flat 2D spatial grid over world space for O(1) territory hit-testing.
/// Rebuilt only when the territory map changes (SSE snapshot/update).
pub struct SpatialGrid {
    cells: Vec<Vec<usize>>,
    names: Vec<String>,
    lefts: Vec<i32>,
    rights: Vec<i32>,
    tops: Vec<i32>,
    bottoms: Vec<i32>,
    min_x: f64,
    min_y: f64,
    cell_w: f64,
    cell_h: f64,
}

impl SpatialGrid {
    pub fn build(territories: &ClientTerritoryMap) -> Self {
        if territories.is_empty() {
            return Self {
                cells: Vec::new(),
                names: Vec::new(),
                lefts: Vec::new(),
                rights: Vec::new(),
                tops: Vec::new(),
                bottoms: Vec::new(),
                min_x: 0.0,
                min_y: 0.0,
                cell_w: 1.0,
                cell_h: 1.0,
            };
        }

        // Compute world bounds
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for ct in territories.values() {
            let loc = &ct.territory.location;
            min_x = min_x.min(loc.left() as f64);
            min_y = min_y.min(loc.top() as f64);
            max_x = max_x.max(loc.right() as f64);
            max_y = max_y.max(loc.bottom() as f64);
        }

        // Add small padding to avoid edge issues
        min_x -= 1.0;
        min_y -= 1.0;
        max_x += 1.0;
        max_y += 1.0;

        let cell_w = (max_x - min_x) / GRID_COLS as f64;
        let cell_h = (max_y - min_y) / GRID_ROWS as f64;

        let mut cells = vec![Vec::new(); GRID_COLS * GRID_ROWS];
        let mut names = Vec::with_capacity(territories.len());
        let mut lefts = Vec::with_capacity(territories.len());
        let mut rights = Vec::with_capacity(territories.len());
        let mut tops = Vec::with_capacity(territories.len());
        let mut bottoms = Vec::with_capacity(territories.len());

        for (idx, (name, ct)) in territories.iter().enumerate() {
            let loc = &ct.territory.location;
            let l = loc.left();
            let r = loc.right();
            let t = loc.top();
            let b = loc.bottom();

            names.push(name.clone());
            lefts.push(l);
            rights.push(r);
            tops.push(t);
            bottoms.push(b);

            // Insert into all overlapping grid cells
            let col_start = ((l as f64 - min_x) / cell_w).floor().max(0.0) as usize;
            let col_end = ((r as f64 - min_x) / cell_w).ceil().min(GRID_COLS as f64) as usize;
            let row_start = ((t as f64 - min_y) / cell_h).floor().max(0.0) as usize;
            let row_end = ((b as f64 - min_y) / cell_h).ceil().min(GRID_ROWS as f64) as usize;

            for row in row_start..row_end {
                for col in col_start..col_end {
                    cells[row * GRID_COLS + col].push(idx);
                }
            }
        }

        Self {
            cells,
            names,
            lefts,
            rights,
            tops,
            bottoms,
            min_x,
            min_y,
            cell_w,
            cell_h,
        }
    }

    /// Returns the world-coordinate bounding box of all territories, or `None` if empty.
    pub fn world_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        if self.cells.is_empty() {
            return None;
        }
        Some((
            self.min_x,
            self.min_y,
            self.min_x + self.cell_w * GRID_COLS as f64,
            self.min_y + self.cell_h * GRID_ROWS as f64,
        ))
    }

    /// The territory at a world coordinate, or else the nearest one whose edge is within
    /// `reach` world units of it. Neighbouring territories in the data are often a few blocks
    /// apart; a point in that seam belongs to the nearer of them, not to neither.
    pub fn find_at(&self, wx: f64, wy: f64, reach: f64) -> Option<String> {
        if self.cells.is_empty() {
            return None;
        }
        let cols = cells_within(wx, reach, self.min_x, self.cell_w, GRID_COLS)?;
        let rows = cells_within(wy, reach, self.min_y, self.cell_h, GRID_ROWS)?;

        // Ties, such as overlapping territories, go to the first one listed.
        let mut nearest: Option<(f64, usize)> = None;
        for row in rows {
            for col in cols.clone() {
                for &idx in &self.cells[row * GRID_COLS + col] {
                    let dx = (self.lefts[idx] as f64 - wx).max(wx - self.rights[idx] as f64);
                    let dy = (self.tops[idx] as f64 - wy).max(wy - self.bottoms[idx] as f64);
                    let distance = dx.max(0.0).hypot(dy.max(0.0));
                    let closer = nearest.is_none_or(|(best, best_idx)| {
                        distance < best || (distance == best && idx < best_idx)
                    });
                    if distance <= reach && closer {
                        nearest = Some((distance, idx));
                    }
                }
            }
        }
        nearest.map(|(_, idx)| self.names[idx].clone())
    }
}

/// The grid cells along one axis that `value ± reach` touches, or `None` when it misses the grid.
fn cells_within(
    value: f64,
    reach: f64,
    origin: f64,
    size: f64,
    count: usize,
) -> Option<std::ops::Range<usize>> {
    let first = ((value - reach - origin) / size).floor();
    let last = ((value + reach - origin) / size).floor();
    if last < 0.0 || first >= count as f64 {
        return None;
    }
    Some(first.max(0.0) as usize..last.min(count as f64 - 1.0) as usize + 1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sequoia_shared::{GuildRef, Region, Territory};

    use super::{GRID_COLS, GRID_ROWS, SpatialGrid};
    use crate::territory::ClientTerritory;

    fn grid(regions: &[(&str, [i32; 2], [i32; 2])]) -> SpatialGrid {
        let territories = regions
            .iter()
            .map(|&(name, start, end)| {
                let territory = Territory {
                    guild: GuildRef {
                        uuid: name.into(),
                        name: name.into(),
                        prefix: "G".into(),
                        color: None,
                    },
                    acquired: chrono::DateTime::UNIX_EPOCH,
                    location: Region { start, end },
                    resources: Default::default(),
                    connections: Vec::new(),
                    runtime: None,
                };
                (
                    name.to_string(),
                    ClientTerritory::from_territory(name, territory),
                )
            })
            .collect::<HashMap<_, _>>();
        SpatialGrid::build(&territories)
    }

    #[test]
    fn a_seam_between_neighbours_belongs_to_the_nearer_one() {
        let grid = grid(&[("West", [0, 0], [100, 100]), ("East", [104, 0], [200, 100])]);
        assert_eq!(grid.find_at(101.0, 50.0, 0.0).as_deref(), None);
        assert_eq!(grid.find_at(101.0, 50.0, 3.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(103.0, 50.0, 3.0).as_deref(), Some("East"));
        assert_eq!(grid.find_at(50.0, 50.0, 3.0).as_deref(), Some("West"));
    }

    #[test]
    fn a_point_beyond_reach_of_every_territory_hits_nothing() {
        let grid = grid(&[("West", [0, 0], [100, 100]), ("East", [130, 0], [200, 100])]);
        assert_eq!(grid.find_at(115.0, 50.0, 3.0).as_deref(), None);
        assert_eq!(grid.find_at(50.0, 104.0, 3.0).as_deref(), None);
        assert_eq!(grid.find_at(102.0, 102.0, 2.0).as_deref(), None);
    }

    #[test]
    fn reach_extends_past_the_edge_of_the_map() {
        let grid = grid(&[("West", [0, 0], [100, 100]), ("East", [104, 0], [200, 100])]);
        assert_eq!(grid.find_at(-2.5, 50.0, 3.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(50.0, 102.5, 3.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(-3.5, 50.0, 3.0).as_deref(), None);
    }

    #[test]
    fn find_at_does_not_truncate_negative_coordinates() {
        let mut cells = vec![Vec::new(); GRID_COLS * GRID_ROWS];
        cells[0].push(0);
        let grid = SpatialGrid {
            cells,
            names: vec!["Alpha".to_string()],
            lefts: vec![0],
            rights: vec![10],
            tops: vec![0],
            bottoms: vec![10],
            min_x: -1.0,
            min_y: 0.0,
            cell_w: 100.0,
            cell_h: 1.0,
        };

        assert_eq!(grid.find_at(-0.2, 0.2, 0.0), None);
        assert_eq!(grid.find_at(0.2, 0.2, 0.0), Some("Alpha".to_string()));
    }
}
