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

        // Listed by name, so overlaps and equal distances resolve the same way on every load
        // rather than in the map's hash order.
        let mut listed: Vec<_> = territories.iter().collect();
        listed.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (idx, (name, ct)) in listed.into_iter().enumerate() {
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

            let cols = cells_spanned(l as f64, r as f64, min_x, cell_w, GRID_COLS);
            let rows = cells_spanned(t as f64, b as f64, min_y, cell_h, GRID_ROWS);
            for row in rows.into_iter().flatten() {
                for col in cols.clone().into_iter().flatten() {
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
        let cols = cells_spanned(wx - reach, wx + reach, self.min_x, self.cell_w, GRID_COLS)?;
        let rows = cells_spanned(wy - reach, wy + reach, self.min_y, self.cell_h, GRID_ROWS)?;

        // A containing territory is at distance 0, so it wins over any within reach; ties,
        // such as overlapping territories, go to the first listed.
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

/// The grid cells along one axis that the closed interval `[lo, hi]` touches, or `None` when
/// it misses the grid. Cell `k` covers `[origin + k·size, origin + (k+1)·size)`, so a
/// territory edge exactly on a cell boundary is in the cell after it too. Territories are
/// filed and points looked up by this one rule, so a lookup sees every territory whose
/// closed rectangle it can reach.
fn cells_spanned(
    lo: f64,
    hi: f64,
    origin: f64,
    size: f64,
    count: usize,
) -> Option<std::ops::Range<usize>> {
    let first = ((lo - origin) / size).floor();
    let last = ((hi - origin) / size).floor();
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
        assert_eq!(grid.find_at(101.0, 50.0, 1.0).as_deref(), Some("West"));
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

    /// Spans x and y from 1 to 399, so the padded grid starts at 0 with 8-unit cells: West's
    /// right and bottom edges (96) and East's and South's near edges (104) lie exactly on
    /// cell boundaries.
    fn on_cell_boundaries() -> SpatialGrid {
        grid(&[
            ("West", [1, 1], [96, 96]),
            ("East", [104, 1], [399, 96]),
            ("South", [1, 104], [96, 399]),
        ])
    }

    #[test]
    fn edges_on_a_cell_boundary_are_inside_their_territory() {
        let grid = on_cell_boundaries();
        assert_eq!(grid.find_at(96.0, 50.0, 0.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(50.0, 96.0, 0.0).as_deref(), Some("West"));
    }

    #[test]
    fn a_reach_exactly_to_an_edge_on_a_cell_boundary_hits_it() {
        let grid = on_cell_boundaries();
        assert_eq!(grid.find_at(97.0, 50.0, 1.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(50.0, 97.0, 1.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(103.0, 50.0, 1.0).as_deref(), Some("East"));
        assert_eq!(grid.find_at(50.0, 103.0, 1.0).as_deref(), Some("South"));
        assert_eq!(grid.find_at(97.5, 50.0, 1.0).as_deref(), None);
    }

    #[test]
    fn equal_distances_across_a_cell_boundary_go_to_the_first_name() {
        let grid = on_cell_boundaries();
        assert_eq!(grid.find_at(100.0, 50.0, 4.0).as_deref(), Some("East"));
        assert_eq!(grid.find_at(50.0, 100.0, 4.0).as_deref(), Some("South"));
    }

    #[test]
    fn a_containing_territory_wins_over_one_within_reach() {
        let grid = on_cell_boundaries();
        assert_eq!(grid.find_at(96.0, 50.0, 8.0).as_deref(), Some("West"));
        assert_eq!(grid.find_at(50.0, 96.0, 8.0).as_deref(), Some("West"));
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
