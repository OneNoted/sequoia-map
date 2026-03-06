use std::collections::BTreeMap;

use crate::territory::ClientTerritoryMap;
use crate::viewport::Viewport;

pub(crate) const CLAIM_LABEL_MIN_SCALE: f64 = 0.10;
pub(crate) const CLAIM_LABEL_MAX_SCALE: f64 = 0.20;
pub(crate) const CLAIM_LABEL_FULL_NAME_MIN_SCALE: f64 = 0.15;
pub(crate) const CLAIM_LABEL_MIN_TERRITORIES: usize = 10;
pub(crate) const CLAIM_LABEL_MIN_SCREEN_WIDTH: f32 = 180.0;
pub(crate) const CLAIM_LABEL_MIN_SCREEN_HEIGHT: f32 = 60.0;
pub(crate) const CLAIM_LABEL_MAX_WIDTH_FRACTION: f32 = 0.82;
pub(crate) const CLAIM_LABEL_FONT_MIN_WORLD: f32 = 36.0;
pub(crate) const CLAIM_LABEL_FONT_MAX_WORLD: f32 = 110.0;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) const CLAIM_LABEL_LETTER_SPACING_EM: f32 = 0.065;

const CLAIM_LABEL_BOUNDS_INSET_PX: f32 = 12.0;
const CLAIM_LABEL_COLLISION_TOLERANCE_PX: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    fn width(self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    fn height(self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    fn area(self) -> f32 {
        self.width() * self.height()
    }

    fn center(self) -> [f32; 2] {
        [
            (self.left + self.right) * 0.5,
            (self.top + self.bottom) * 0.5,
        ]
    }

    fn inset(self, amount: f32) -> Option<Self> {
        let inset = amount.max(0.0);
        let inset_rect = Self {
            left: self.left + inset,
            top: self.top + inset,
            right: self.right - inset,
            bottom: self.bottom - inset,
        };
        if inset_rect.width() <= 0.0 || inset_rect.height() <= 0.0 {
            None
        } else {
            Some(inset_rect)
        }
    }

    fn contains_rect(self, other: Self) -> bool {
        other.left >= self.left
            && other.top >= self.top
            && other.right <= self.right
            && other.bottom <= self.bottom
    }

    fn overlaps_more_than(self, other: Self, tolerance: f32) -> bool {
        let overlap_x = (self.right.min(other.right) - self.left.max(other.left)).max(0.0);
        let overlap_y = (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0.0);
        overlap_x > tolerance && overlap_y > tolerance
    }

    fn from_center_size(center: [f32; 2], width: f32, height: f32) -> Self {
        let half_w = width * 0.5;
        let half_h = height * 0.5;
        Self {
            left: center[0] - half_w,
            top: center[1] - half_h,
            right: center[0] + half_w,
            bottom: center[1] + half_h,
        }
    }

    fn to_screen(self, vp: &Viewport) -> Self {
        let (left, top) = vp.world_to_screen(self.left as f64, self.top as f64);
        let (right, bottom) = vp.world_to_screen(self.right as f64, self.bottom as f64);
        Self {
            left: left as f32,
            top: top as f32,
            right: right as f32,
            bottom: bottom as f32,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClaimCluster {
    pub guild_name: String,
    pub guild_prefix: String,
    pub guild_color: (u8, u8, u8),
    pub territory_count: usize,
    pub bounds_world: Rect,
    pub centroid_world: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClaimLabelCandidate {
    pub text: String,
    pub guild_color: (u8, u8, u8),
    pub territory_count: usize,
    pub center_world: [f32; 2],
    pub font_height_world: f32,
    pub max_width_world: f32,
    pub text_bounds_world: Rect,
    pub text_bounds_screen: Rect,
}

#[derive(Clone, Debug)]
struct TerritoryNode {
    territory_name: String,
    guild_name: String,
    guild_prefix: String,
    guild_color: (u8, u8, u8),
    bounds_world: Rect,
}

pub(crate) fn claim_label_zoom_active(scale: f64) -> bool {
    scale >= CLAIM_LABEL_MIN_SCALE && scale <= CLAIM_LABEL_MAX_SCALE
}

pub(crate) fn build_claim_clusters(territories: &ClientTerritoryMap) -> Vec<ClaimCluster> {
    let mut by_guild: BTreeMap<String, Vec<TerritoryNode>> = BTreeMap::new();
    for (territory_name, ct) in territories {
        let guild = &ct.territory.guild;
        let guild_key = if guild.uuid.trim().is_empty() {
            guild.name.clone()
        } else {
            guild.uuid.clone()
        };
        let location = &ct.territory.location;
        by_guild.entry(guild_key).or_default().push(TerritoryNode {
            territory_name: territory_name.clone(),
            guild_name: guild.name.clone(),
            guild_prefix: guild.prefix.clone(),
            guild_color: ct.guild_color,
            bounds_world: Rect {
                left: location.left() as f32,
                top: location.top() as f32,
                right: location.right() as f32,
                bottom: location.bottom() as f32,
            },
        });
    }

    let mut clusters = Vec::new();
    for mut nodes in by_guild.into_values() {
        nodes.sort_by(|a, b| a.territory_name.cmp(&b.territory_name));
        let mut visited = vec![false; nodes.len()];
        for start in 0..nodes.len() {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![start];
            let mut component = Vec::new();
            while let Some(idx) = stack.pop() {
                component.push(idx);
                for next in 0..nodes.len() {
                    if visited[next]
                        || !rectangles_share_claim_edge(
                            nodes[idx].bounds_world,
                            nodes[next].bounds_world,
                        )
                    {
                        continue;
                    }
                    visited[next] = true;
                    stack.push(next);
                }
            }
            clusters.push(build_cluster(&nodes, &component));
        }
    }

    clusters.sort_by(|a, b| {
        a.guild_name
            .cmp(&b.guild_name)
            .then_with(|| a.bounds_world.left.total_cmp(&b.bounds_world.left))
            .then_with(|| a.bounds_world.top.total_cmp(&b.bounds_world.top))
            .then_with(|| a.territory_count.cmp(&b.territory_count))
    });
    clusters
}

pub(crate) fn select_claim_label_candidates<F>(
    clusters: &[ClaimCluster],
    vp: &Viewport,
    line_height_units: f32,
    measure_units: F,
) -> Vec<ClaimLabelCandidate>
where
    F: Fn(&str) -> f32,
{
    if !claim_label_zoom_active(vp.scale) || line_height_units <= 0.0 {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for cluster in clusters {
        if cluster.territory_count < CLAIM_LABEL_MIN_TERRITORIES {
            continue;
        }

        let cluster_screen_rect = cluster.bounds_world.to_screen(vp);
        if cluster_screen_rect.width() < CLAIM_LABEL_MIN_SCREEN_WIDTH
            || cluster_screen_rect.height() < CLAIM_LABEL_MIN_SCREEN_HEIGHT
        {
            continue;
        }

        let max_width_world = cluster.bounds_world.width() * CLAIM_LABEL_MAX_WIDTH_FRACTION;
        if max_width_world <= 0.0 {
            continue;
        }

        let font_height_world = claim_font_height_world(cluster.bounds_world);
        let prefix = cluster.guild_prefix.trim();
        let full_name = cluster.guild_name.trim();
        let fallback = if prefix.is_empty() { full_name } else { prefix };
        if fallback.is_empty() {
            continue;
        }

        let text = if vp.scale >= CLAIM_LABEL_FULL_NAME_MIN_SCALE
            && !full_name.is_empty()
            && full_name != fallback
            && text_fits_without_scaling(
                full_name,
                font_height_world,
                max_width_world,
                line_height_units,
                &measure_units,
            ) {
            full_name
        } else {
            fallback
        };

        let Some((text_width_world, text_height_world)) = fitted_text_box_world(
            text,
            font_height_world,
            max_width_world,
            line_height_units,
            &measure_units,
        ) else {
            continue;
        };

        let inset_world = CLAIM_LABEL_BOUNDS_INSET_PX / vp.scale.max(f64::EPSILON) as f32;
        let Some(safe_bounds_world) = cluster.bounds_world.inset(inset_world) else {
            continue;
        };
        if text_width_world > safe_bounds_world.width()
            || text_height_world > safe_bounds_world.height()
        {
            continue;
        }

        let min_center = [
            safe_bounds_world.left + text_width_world * 0.5,
            safe_bounds_world.top + text_height_world * 0.5,
        ];
        let max_center = [
            safe_bounds_world.right - text_width_world * 0.5,
            safe_bounds_world.bottom - text_height_world * 0.5,
        ];
        if min_center[0] > max_center[0] || min_center[1] > max_center[1] {
            continue;
        }

        let center_world = [
            cluster.centroid_world[0].clamp(min_center[0], max_center[0]),
            cluster.centroid_world[1].clamp(min_center[1], max_center[1]),
        ];
        let text_bounds_world =
            Rect::from_center_size(center_world, text_width_world, text_height_world);
        let text_bounds_screen = text_bounds_world.to_screen(vp);
        let Some(safe_bounds_screen) = cluster_screen_rect.inset(CLAIM_LABEL_BOUNDS_INSET_PX)
        else {
            continue;
        };
        if !safe_bounds_screen.contains_rect(text_bounds_screen) {
            continue;
        }

        candidates.push(ClaimLabelCandidate {
            text: text.to_string(),
            guild_color: cluster.guild_color,
            territory_count: cluster.territory_count,
            center_world,
            font_height_world,
            max_width_world,
            text_bounds_world,
            text_bounds_screen,
        });
    }

    candidates.sort_by(|a, b| {
        b.territory_count
            .cmp(&a.territory_count)
            .then_with(|| {
                b.text_bounds_screen
                    .area()
                    .total_cmp(&a.text_bounds_screen.area())
            })
            .then_with(|| a.text.cmp(&b.text))
    });

    let mut accepted = Vec::new();
    for candidate in candidates {
        let overlaps_existing = accepted.iter().any(|existing: &ClaimLabelCandidate| {
            existing.text_bounds_screen.overlaps_more_than(
                candidate.text_bounds_screen,
                CLAIM_LABEL_COLLISION_TOLERANCE_PX,
            )
        });
        if !overlaps_existing {
            accepted.push(candidate);
        }
    }
    accepted
}

fn build_cluster(nodes: &[TerritoryNode], component: &[usize]) -> ClaimCluster {
    let mut left = f32::INFINITY;
    let mut top = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    let mut area_sum = 0.0f32;
    let mut centroid_x = 0.0f32;
    let mut centroid_y = 0.0f32;

    for idx in component {
        let rect = nodes[*idx].bounds_world;
        left = left.min(rect.left);
        top = top.min(rect.top);
        right = right.max(rect.right);
        bottom = bottom.max(rect.bottom);
        let area = rect.area().max(1.0);
        let center = rect.center();
        area_sum += area;
        centroid_x += center[0] * area;
        centroid_y += center[1] * area;
    }

    let first = &nodes[component[0]];
    ClaimCluster {
        guild_name: first.guild_name.clone(),
        guild_prefix: first.guild_prefix.clone(),
        guild_color: first.guild_color,
        territory_count: component.len(),
        bounds_world: Rect {
            left,
            top,
            right,
            bottom,
        },
        centroid_world: if area_sum > 0.0 {
            [centroid_x / area_sum, centroid_y / area_sum]
        } else {
            [(left + right) * 0.5, (top + bottom) * 0.5]
        },
    }
}

fn claim_font_height_world(bounds_world: Rect) -> f32 {
    (bounds_world.height() * 0.58)
        .min(bounds_world.width() * 0.16)
        .clamp(CLAIM_LABEL_FONT_MIN_WORLD, CLAIM_LABEL_FONT_MAX_WORLD)
}

fn text_fits_without_scaling<F>(
    text: &str,
    font_height_world: f32,
    max_width_world: f32,
    line_height_units: f32,
    measure_units: &F,
) -> bool
where
    F: Fn(&str) -> f32,
{
    let natural_width_world = measure_units(text) * (font_height_world / line_height_units);
    natural_width_world <= max_width_world
}

fn fitted_text_box_world<F>(
    text: &str,
    font_height_world: f32,
    max_width_world: f32,
    line_height_units: f32,
    measure_units: &F,
) -> Option<(f32, f32)>
where
    F: Fn(&str) -> f32,
{
    if text.is_empty()
        || font_height_world <= 0.0
        || max_width_world <= 0.0
        || line_height_units <= 0.0
    {
        return None;
    }
    let units = measure_units(text);
    if units <= 0.0 {
        return None;
    }
    let natural_scale = font_height_world / line_height_units;
    let natural_width_world = units * natural_scale;
    let fit_scale = if natural_width_world > max_width_world {
        (max_width_world / natural_width_world).clamp(0.2, 1.0)
    } else {
        1.0
    };
    Some((
        natural_width_world * fit_scale,
        font_height_world * fit_scale,
    ))
}

fn rectangles_share_claim_edge(a: Rect, b: Rect) -> bool {
    let overlap_x = (a.right.min(b.right) - a.left.max(b.left)).max(0.0);
    let overlap_y = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0.0);
    let touch_x =
        (a.right - b.left).abs() < f32::EPSILON || (b.right - a.left).abs() < f32::EPSILON;
    let touch_y =
        (a.bottom - b.top).abs() < f32::EPSILON || (b.bottom - a.top).abs() < f32::EPSILON;

    (overlap_x > 0.0 && touch_y)
        || (overlap_y > 0.0 && touch_x)
        || (overlap_x > 0.0 && overlap_y > 0.0)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sequoia_shared::{GuildRef, Region, Resources, Territory};

    use super::{
        CLAIM_LABEL_FONT_MAX_WORLD, ClaimCluster, build_claim_clusters,
        select_claim_label_candidates,
    };
    use crate::territory::{ClientTerritory, ClientTerritoryMap};
    use crate::viewport::Viewport;

    fn make_map(entries: &[(&str, &str, &str, [i32; 4])]) -> ClientTerritoryMap {
        let mut map = ClientTerritoryMap::new();
        for (territory_name, guild_name, guild_prefix, [left, top, right, bottom]) in entries {
            let territory = Territory {
                guild: GuildRef {
                    uuid: format!("guild-{guild_name}"),
                    name: (*guild_name).to_string(),
                    prefix: (*guild_prefix).to_string(),
                    color: None,
                },
                acquired: Utc::now(),
                location: Region {
                    start: [*left, *top],
                    end: [*right, *bottom],
                },
                resources: Resources::default(),
                connections: Vec::new(),
                runtime: None,
            };
            map.insert(
                (*territory_name).to_string(),
                ClientTerritory::from_territory(territory_name, territory),
            );
        }
        map
    }

    fn cluster(
        guild_name: &str,
        guild_prefix: &str,
        territory_count: usize,
        bounds: [f32; 4],
    ) -> ClaimCluster {
        ClaimCluster {
            guild_name: guild_name.to_string(),
            guild_prefix: guild_prefix.to_string(),
            guild_color: (120, 90, 200),
            territory_count,
            bounds_world: super::Rect {
                left: bounds[0],
                top: bounds[1],
                right: bounds[2],
                bottom: bounds[3],
            },
            centroid_world: [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5],
        }
    }

    fn measure_units(text: &str) -> f32 {
        text.chars().count() as f32 * 10.0
    }

    #[test]
    fn build_claim_clusters_merges_side_adjacent_territories() {
        let map = make_map(&[
            ("A", "Nia", "NIA", [0, 0, 10, 10]),
            ("B", "Nia", "NIA", [10, 0, 20, 10]),
        ]);

        let clusters = build_claim_clusters(&map);

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].territory_count, 2);
        assert_eq!(clusters[0].bounds_world.left, 0.0);
        assert_eq!(clusters[0].bounds_world.right, 20.0);
        assert_eq!(clusters[0].bounds_world.top, 0.0);
        assert_eq!(clusters[0].bounds_world.bottom, 10.0);
    }

    #[test]
    fn build_claim_clusters_keeps_disconnected_regions_separate() {
        let map = make_map(&[
            ("A", "Nia", "NIA", [0, 0, 10, 10]),
            ("B", "Nia", "NIA", [30, 0, 40, 10]),
        ]);

        let clusters = build_claim_clusters(&map);

        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|cluster| cluster.territory_count == 1));
    }

    #[test]
    fn build_claim_clusters_does_not_merge_corner_touching_territories() {
        let map = make_map(&[
            ("A", "Nia", "NIA", [0, 0, 10, 10]),
            ("B", "Nia", "NIA", [10, 10, 20, 20]),
        ]);

        let clusters = build_claim_clusters(&map);

        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn claim_labels_require_large_enough_clusters() {
        let vp = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.16,
        };
        let clusters = vec![cluster(
            "Aurora Dominion",
            "AUR",
            9,
            [0.0, 0.0, 1400.0, 500.0],
        )];

        let labels = select_claim_label_candidates(&clusters, &vp, 10.0, measure_units);

        assert!(labels.is_empty());
    }

    #[test]
    fn claim_labels_use_prefix_below_full_name_zoom_threshold() {
        let vp = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.14,
        };
        let clusters = vec![cluster("Aurora", "AUR", 12, [0.0, 0.0, 1400.0, 500.0])];

        let labels = select_claim_label_candidates(&clusters, &vp, 10.0, measure_units);

        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "AUR");
        assert!(labels[0].font_height_world <= CLAIM_LABEL_FONT_MAX_WORLD);
    }

    #[test]
    fn claim_labels_use_full_name_when_it_fits_at_closer_zoom() {
        let vp = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.16,
        };
        let clusters = vec![cluster("Aurora", "AUR", 12, [0.0, 0.0, 1400.0, 500.0])];

        let labels = select_claim_label_candidates(&clusters, &vp, 10.0, measure_units);

        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "Aurora");
    }

    #[test]
    fn claim_labels_fall_back_to_prefix_when_full_name_is_too_wide() {
        let vp = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.16,
        };
        let clusters = vec![cluster(
            "The Long Dominion Collective",
            "TLDC",
            12,
            [0.0, 0.0, 1400.0, 500.0],
        )];

        let labels = select_claim_label_candidates(&clusters, &vp, 10.0, measure_units);

        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "TLDC");
    }

    #[test]
    fn larger_claim_keeps_label_when_candidates_overlap() {
        let vp = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.14,
        };
        let clusters = vec![
            cluster("Magnus", "MAGNUS", 12, [0.0, 0.0, 1400.0, 500.0]),
            cluster("Citadel", "CITADEL", 10, [500.0, 0.0, 1900.0, 500.0]),
        ];

        let labels = select_claim_label_candidates(&clusters, &vp, 10.0, measure_units);

        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "MAGNUS");
        assert_eq!(labels[0].territory_count, 12);
    }
}
