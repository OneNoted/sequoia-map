//! Map Intel point markers: gathering nodes, their per-area summaries, and world-event,
//! raid and camp sites. The renderer draws them over the map at a constant screen size
//! around their world positions; this module owns which of them show at a zoom and how big
//! they are.

use std::ops::Range;

/// Below this scale each area's nodes show as one summary marker per profession.
pub const NODE_CLUSTER_SCALE: f64 = 0.18;
/// Below this scale nodes are plain squares; from it on they show their node shape.
pub const NODE_SIMPLE_SCALE: f64 = 0.34;
const NODE_MIN_RADIUS: f64 = 1.25;
const NODE_MAX_RADIUS: f64 = 3.25;
/// Nodes are kept in square world cells of this size, so a frame only draws those in view.
const NODE_CELL_WORLD_SIZE: f32 = 256.0;

/// Side of a summary marker, CSS pixels.
const SUMMARY_SIZE: f32 = 3.0;
/// Site marker radii and their outline, CSS pixels.
const EVENT_RADIUS: f32 = 6.0;
const RAID_RADIUS: f32 = 6.0;
const CAMP_RADIUS: f32 = 7.0;
const SITE_STROKE_WIDTH: f32 = 2.0;
const SITE_STROKE_RGBA: [f32; 4] = [12.0 / 255.0, 14.0 / 255.0, 23.0 / 255.0, 0.9];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerShape {
    /// A small square standing for several nodes.
    Summary,
    Dot,
    /// A square.
    Corner,
    /// A cross.
    Wall,
    /// An outlined diamond.
    Event,
    /// An outlined square.
    Raid,
    /// An outlined triangle.
    Camp,
}

impl MarkerShape {
    /// The shape's code in the renderer's marker shader.
    pub fn code(self) -> u32 {
        match self {
            MarkerShape::Summary => 0,
            MarkerShape::Dot => 1,
            MarkerShape::Corner => 2,
            MarkerShape::Wall => 3,
            MarkerShape::Event => 4,
            MarkerShape::Raid => 5,
            MarkerShape::Camp => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapMarker {
    pub world: [f32; 2],
    pub shape: MarkerShape,
    pub rgb: [u8; 3],
}

/// Every marker of one overlay, in drawing order within each group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapMarkers {
    /// Grouped by world cell; each cell keeps its nodes' given order.
    nodes: Vec<MapMarker>,
    /// Column, row and first node of each cell, in node order.
    node_cells: Vec<(i32, i32, u32)>,
    summaries: Vec<MapMarker>,
    sites: Vec<MapMarker>,
}

impl MapMarkers {
    /// `sites` are drawn over the gathering markers.
    pub fn new(
        mut nodes: Vec<MapMarker>,
        summaries: Vec<MapMarker>,
        sites: Vec<MapMarker>,
    ) -> Self {
        nodes.sort_by_key(|node| node_cell(node.world));
        let mut node_cells: Vec<(i32, i32, u32)> = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            let (column, row) = node_cell(node.world);
            if node_cells
                .last()
                .is_none_or(|&(last_column, last_row, _)| (last_column, last_row) != (column, row))
            {
                node_cells.push((column, row, index as u32));
            }
        }
        Self {
            nodes,
            node_cells,
            summaries,
            sites,
        }
    }

    pub fn nodes(&self) -> &[MapMarker] {
        &self.nodes
    }

    pub fn summaries(&self) -> &[MapMarker] {
        &self.summaries
    }

    pub fn sites(&self) -> &[MapMarker] {
        &self.sites
    }

    /// Index ranges into [`Self::nodes`] covering every node inside the world rectangle,
    /// cell by cell (neighbouring cells merged); nodes of other cells are left out.
    pub fn node_ranges_in(&self, min: [f32; 2], max: [f32; 2]) -> Vec<Range<u32>> {
        let (min_column, min_row) = node_cell(min);
        let (max_column, max_row) = node_cell(max);
        let mut ranges: Vec<Range<u32>> = Vec::new();
        for (index, &(column, row, start)) in self.node_cells.iter().enumerate() {
            if column < min_column || column > max_column || row < min_row || row > max_row {
                continue;
            }
            let end = self
                .node_cells
                .get(index + 1)
                .map_or(self.nodes.len() as u32, |&(_, _, next)| next);
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        ranges
    }
}

fn node_cell(world: [f32; 2]) -> (i32, i32) {
    (
        (world[0] / NODE_CELL_WORLD_SIZE).floor() as i32,
        (world[1] / NODE_CELL_WORLD_SIZE).floor() as i32,
    )
}

pub fn shows_node_summaries(scale: f64) -> bool {
    scale < NODE_CLUSTER_SCALE
}

/// Radius of a node marker at a scale, CSS pixels.
pub fn node_radius(scale: f64) -> f64 {
    (1.4 + scale * 0.9).clamp(NODE_MIN_RADIUS, NODE_MAX_RADIUS)
}

/// Sizes the marker shader needs for a frame, CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerStyle {
    pub node_radius: f32,
    /// Side of the plain square every node is drawn as, or 0 where nodes show their shape.
    pub simple_node_size: f32,
    pub summary_size: f32,
    pub event_radius: f32,
    pub raid_radius: f32,
    pub camp_radius: f32,
    pub site_stroke_width: f32,
    pub site_stroke_rgba: [f32; 4],
}

pub fn marker_style(scale: f64) -> MarkerStyle {
    let radius = node_radius(scale);
    MarkerStyle {
        node_radius: radius as f32,
        simple_node_size: if scale < NODE_SIMPLE_SCALE {
            (radius * 1.65).max(2.0) as f32
        } else {
            0.0
        },
        summary_size: SUMMARY_SIZE,
        event_radius: EVENT_RADIUS,
        raid_radius: RAID_RADIUS,
        camp_radius: CAMP_RADIUS,
        site_stroke_width: SITE_STROKE_WIDTH,
        site_stroke_rgba: SITE_STROKE_RGBA,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(x: f32, z: f32, tag: u8) -> MapMarker {
        MapMarker {
            world: [x, z],
            shape: MarkerShape::Dot,
            rgb: [tag, 0, 0],
        }
    }

    fn tags(markers: &MapMarkers, ranges: &[Range<u32>]) -> Vec<u8> {
        ranges
            .iter()
            .flat_map(|range| &markers.nodes()[range.start as usize..range.end as usize])
            .map(|node| node.rgb[0])
            .collect()
    }

    #[test]
    fn zoomed_out_shows_summaries_instead_of_nodes() {
        assert!(shows_node_summaries(0.1));
        assert!(shows_node_summaries(0.1799));
        assert!(!shows_node_summaries(NODE_CLUSTER_SCALE));
        assert!(!shows_node_summaries(2.0));
    }

    #[test]
    fn nodes_are_grouped_by_cell_in_their_given_order() {
        let markers = MapMarkers::new(
            vec![
                node(600.0, 0.0, 1),
                node(10.0, 10.0, 2),
                node(-10.0, 0.0, 3),
                node(20.0, 20.0, 4),
            ],
            Vec::new(),
            Vec::new(),
        );
        let order: Vec<u8> = markers.nodes().iter().map(|node| node.rgb[0]).collect();
        assert_eq!(order, [3, 2, 4, 1]);
    }

    #[test]
    fn only_nodes_of_cells_in_view_are_drawn() {
        // Cells are 256 world units: columns -1, 0, 0, 1, 2 and rows 0 or 1.
        let markers = MapMarkers::new(
            vec![
                node(-10.0, 10.0, 1),
                node(10.0, 10.0, 2),
                node(10.0, 300.0, 3),
                node(300.0, 10.0, 4),
                node(600.0, 10.0, 5),
            ],
            Vec::new(),
            Vec::new(),
        );
        // Cells (0, 0) and (1, 0); cell (0, 1) between them is out of view.
        let ranges = markers.node_ranges_in([0.0, 0.0], [400.0, 100.0]);
        assert_eq!(ranges, [1..2, 3..4]);
        assert_eq!(tags(&markers, &ranges), [2, 4]);
        // Columns 0..=1, rows 0..=1.
        let ranges = markers.node_ranges_in([5.0, 5.0], [300.0, 300.0]);
        assert_eq!(tags(&markers, &ranges), [2, 3, 4]);
        assert_eq!((ranges.len(), ranges[0].clone()), (1, 1..4));
        // Everything.
        let ranges = markers.node_ranges_in([-1000.0, -1000.0], [1000.0, 1000.0]);
        assert_eq!((ranges.len(), ranges[0].clone()), (1, 0..5));
        // Nothing.
        assert!(
            markers
                .node_ranges_in([2000.0, 0.0], [2100.0, 10.0])
                .is_empty()
        );
    }

    #[test]
    fn nodes_are_plain_squares_until_they_show_their_shape() {
        // The sizes the canvas overlay drew: squares of 1.65 radii (at least 2 px), then
        // shapes of the node radius, clamped to 1.25..=3.25 px.
        let style = marker_style(0.2);
        assert!((style.node_radius - 1.58).abs() < 1e-6);
        assert!((style.simple_node_size - 1.58 * 1.65).abs() < 1e-5);
        assert_eq!(marker_style(NODE_SIMPLE_SCALE).simple_node_size, 0.0);
        assert!((marker_style(0.5).node_radius - 1.85).abs() < 1e-6);
        assert_eq!(marker_style(4.0).node_radius, 3.25);
        assert_eq!(marker_style(0.0).node_radius, 1.4);
        assert_eq!(marker_style(0.1).summary_size, 3.0);
    }

    #[test]
    fn shape_codes_are_distinct() {
        let shapes = [
            MarkerShape::Summary,
            MarkerShape::Dot,
            MarkerShape::Corner,
            MarkerShape::Wall,
            MarkerShape::Event,
            MarkerShape::Raid,
            MarkerShape::Camp,
        ];
        let mut codes: Vec<u32> = shapes.iter().map(|shape| shape.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes, (0..7).collect::<Vec<u32>>());
    }
}
