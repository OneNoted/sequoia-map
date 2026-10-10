//! Connection lines between neighbouring territories: which edges are drawn, in what colour,
//! how wide and how opaque, as world-space vertices for the renderer.
//!
//! Widths are CSS pixels on screen. Classic hairlines are spread in world units at the main
//! camera's scale, so they are rebuilt whenever it changes (on the minimap's much smaller scale
//! they collapse into a single hairline, as they always have). Solid strips instead carry
//! their half-width as a screen-space offset that the shader adds after projecting the
//! centreline, so the same vertices are exactly as wide under any projection: the main map at
//! any zoom and the minimap alike, without a rebuild when the main camera zooms. The
//! [`ConnectionStyle`]s:
//!
//! - Classic, the original look and the default. Each connection is a band of hairlines
//!   (one device pixel each) 0.6 CSS px apart, 0.8 when bold, fading from the middle out:
//!   faint white, or tinted with the guild colour and stronger when bold. The band also fades
//!   out as the map zooms out. Thickness widens the band by adding hairlines rather than
//!   spreading the same five apart, so wider bands keep the same texture instead of falling
//!   apart into separate lines; at 100% thickness it is the original five hairlines.
//!   Opacity multiplies the classic opacities.
//! - Solid white and solid guild colour. Each connection is a filled strip of one colour and
//!   one opacity, 1.5 CSS px wide (3 px when bold) times the thickness, with no fade across
//!   it or with zoom.
//!
//! Wherever a guild colour is used, an edge between territories of different guilds is drawn
//! in two halves meeting at its midpoint, each in its own territory's colour. (Which end a
//! hash map happened to visit first used to decide.)

use std::collections::HashSet;

use crate::colors::brighten;
use crate::settings::{ConnectionStyle, RenderSettings};
use crate::territory::{ClientTerritory, ClientTerritoryMap};

/// Hairline offsets and opacities across a classic band at 100% thickness, outermost first
/// up to the middle (CSS px, opacity factor).
const CLASSIC_PROFILE: [(f32, f32); 3] = [(1.2, 0.28), (0.6, 0.6), (0.0, 1.0)];
const CLASSIC_BOLD_PROFILE: [(f32, f32); 3] = [(1.6, 0.45), (0.8, 0.75), (0.0, 1.0)];
/// Middle opacity of a classic, non-bold band.
const CLASSIC_WHITE_ALPHA: f32 = 0.16;
/// Width of a solid line at 100% thickness, in CSS px.
pub const SOLID_WIDTH_PX: f32 = 1.5;
/// Bold multiplies a solid line's width by this.
pub const SOLID_BOLD_FACTOR: f32 = 2.0;
/// Thickness and width bounds whatever a host passes in.
const MIN_THICKNESS_SCALE: f32 = 0.2;
const MAX_THICKNESS_SCALE: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConnectionVertex {
    pub world: [f32; 2],
    /// CSS pixels added after projecting `world`: a solid strip's half-width, zero for
    /// classic hairlines.
    pub offset: [f32; 2],
    pub color: [f32; 4],
}

/// The connection vertices for one camera scale and set of settings.
#[derive(Debug, Default)]
pub struct ConnectionMesh {
    /// Vertex pairs for a line list: classic hairlines.
    pub lines: Vec<ConnectionVertex>,
    /// Vertex triples for a triangle list: solid strips.
    pub triangles: Vec<ConnectionVertex>,
    seen: HashSet<(u64, u64)>,
}

impl ConnectionMesh {
    pub fn rebuild(
        &mut self,
        territories: &ClientTerritoryMap,
        scale: f64,
        settings: &RenderSettings,
    ) {
        self.lines.clear();
        self.triangles.clear();
        self.seen.clear();
        if !settings.show_connections {
            return;
        }
        let style = Style::new(settings, scale);
        if style.alpha < 0.001 {
            return;
        }
        for from in territories.values() {
            for name in &from.territory.connections {
                let Some(to) = territories.get(name) else {
                    continue;
                };
                let edge = if from.name_hash < to.name_hash {
                    (from.name_hash, to.name_hash)
                } else {
                    (to.name_hash, from.name_hash)
                };
                if self.seen.insert(edge) {
                    self.push_edge(&style, from, to);
                }
            }
        }
    }

    fn push_edge(&mut self, style: &Style, from: &ClientTerritory, to: &ClientTerritory) {
        let a = midpoint(from);
        let b = midpoint(to);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let length = dx.hypot(dy);
        if length <= f32::EPSILON {
            return;
        }
        // Unit normal, and scaled to world units per CSS pixel at the main camera's scale.
        let unit = [-dy / length, dx / length];
        let normal = [unit[0] * style.world_per_px, unit[1] * style.world_per_px];
        let (color_a, color_b) = (style.color(from), style.color(to));
        let halves: &[([f32; 2], [f32; 2], [f32; 4])] = if color_a == color_b {
            &[(a, b, color_a)]
        } else {
            let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
            &[(a, mid, color_a), (mid, b, color_b)]
        };
        for &(start, end, color) in halves {
            match style.kind {
                ConnectionStyle::Classic => {
                    for (offset, factor) in &style.hairlines {
                        let shift = [normal[0] * offset, normal[1] * offset];
                        let color = with_alpha(color, color[3] * factor);
                        self.lines.push(vertex(start, shift, [0.0, 0.0], color));
                        self.lines.push(vertex(end, shift, [0.0, 0.0], color));
                    }
                }
                ConnectionStyle::White | ConnectionStyle::Guild => {
                    let half = style.solid_width_px * 0.5;
                    let up = [unit[0] * half, unit[1] * half];
                    let down = [-up[0], -up[1]];
                    let corners = [
                        vertex(start, [0.0, 0.0], up, color),
                        vertex(end, [0.0, 0.0], up, color),
                        vertex(end, [0.0, 0.0], down, color),
                        vertex(start, [0.0, 0.0], down, color),
                    ];
                    self.triangles.extend([
                        corners[0], corners[1], corners[2], corners[0], corners[2], corners[3],
                    ]);
                }
            }
        }
    }
}

/// Everything about a rebuild that does not depend on the edge.
struct Style {
    kind: ConnectionStyle,
    bold: bool,
    /// Opacity applied to every line before per-colour and per-hairline factors.
    alpha: f32,
    /// World units per CSS pixel at the main camera's scale; classic hairlines only.
    world_per_px: f32,
    /// Classic: (offset in CSS px, opacity factor) of every hairline across the band.
    hairlines: Vec<(f32, f32)>,
    /// Solid: full width in CSS px.
    solid_width_px: f32,
}

impl Style {
    fn new(settings: &RenderSettings, scale: f64) -> Self {
        let kind = settings.connection_style;
        let bold = settings.bold_connections;
        let thickness = finite_or(settings.connection_thickness_scale, 1.0)
            .clamp(MIN_THICKNESS_SCALE, MAX_THICKNESS_SCALE);
        let alpha = match kind {
            ConnectionStyle::Classic => {
                let (from, to) = settings.connection_zoom_fade;
                smoothstep(from, to, scale as f32)
                    * finite_or(settings.connection_opacity_scale, 1.0).max(0.0)
            }
            ConnectionStyle::White | ConnectionStyle::Guild => {
                finite_or(settings.connection_solid_opacity, 1.0).clamp(0.0, 1.0)
            }
        };
        let profile = if bold {
            &CLASSIC_BOLD_PROFILE
        } else {
            &CLASSIC_PROFILE
        };
        let solid_bold = if bold { SOLID_BOLD_FACTOR } else { 1.0 };
        Self {
            kind,
            bold,
            alpha,
            world_per_px: (1.0 / (scale as f32).max(0.05)).min(24.0),
            hairlines: if kind == ConnectionStyle::Classic {
                classic_hairlines(profile, thickness)
            } else {
                Vec::new()
            },
            solid_width_px: SOLID_WIDTH_PX * solid_bold * thickness,
        }
    }

    fn color(&self, territory: &ClientTerritory) -> [f32; 4] {
        let (r, g, b) = territory.guild_color;
        match self.kind {
            ConnectionStyle::Classic if self.bold => {
                // Darker guild colours are lifted more and drawn a little stronger.
                let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
                let dark_boost = (1.0 - luminance / 255.0).clamp(0.0, 1.0);
                let (r, g, b) = brighten(r, g, b, 1.4 + dark_boost * 0.8);
                rgba(r, g, b, (0.35 + dark_boost * 0.20) as f32 * self.alpha)
            }
            ConnectionStyle::Classic => [1.0, 1.0, 1.0, CLASSIC_WHITE_ALPHA * self.alpha],
            ConnectionStyle::White => [1.0, 1.0, 1.0, self.alpha],
            ConnectionStyle::Guild => rgba(r, g, b, self.alpha),
        }
    }
}

/// Hairlines spanning the classic band at `thickness`: no further apart than the profile's
/// own spacing, with opacities interpolated along the profile.
fn classic_hairlines(profile: &[(f32, f32); 3], thickness: f32) -> Vec<(f32, f32)> {
    let spacing = profile[1].0 - profile[2].0;
    let half_width = profile[0].0 * thickness;
    let per_side = (half_width / spacing - 1e-4).ceil().max(1.0) as i32;
    let step = half_width / per_side as f32;
    (-per_side..=per_side)
        .map(|i| {
            let u = (i.abs() as f32) / per_side as f32;
            (step * i as f32, profile_factor(profile, u))
        })
        .collect()
}

/// Opacity factor at `u` from the middle (0) to the edge (1) of a classic profile.
fn profile_factor(profile: &[(f32, f32); 3], u: f32) -> f32 {
    let (edge, inner, middle) = (profile[0].1, profile[1].1, profile[2].1);
    if u <= 0.5 {
        middle + (inner - middle) * (u / 0.5)
    } else {
        inner + (edge - inner) * ((u - 0.5) / 0.5)
    }
}

fn midpoint(territory: &ClientTerritory) -> [f32; 2] {
    let location = &territory.territory.location;
    [location.midpoint_x() as f32, location.midpoint_y() as f32]
}

fn vertex(at: [f32; 2], shift: [f32; 2], offset: [f32; 2], color: [f32; 4]) -> ConnectionVertex {
    ConnectionVertex {
        world: [at[0] + shift[0], at[1] + shift[1]],
        offset,
        color,
    }
}

fn rgba(r: u8, g: u8, b: u8, alpha: f32) -> [f32; 4] {
    [
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        alpha.clamp(0.0, 1.0),
    ]
}

fn with_alpha(color: [f32; 4], alpha: f32) -> [f32; 4] {
    [color[0], color[1], color[2], alpha.clamp(0.0, 1.0)]
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sequoia_shared::{GuildRef, Region, Territory};

    use super::*;
    use crate::settings::tests::settings;

    const RED: (u8, u8, u8) = (200, 40, 40);
    const BLUE: (u8, u8, u8) = (30, 60, 220);

    fn territory(at: (i32, i32), color: (u8, u8, u8), connections: &[&str]) -> Territory {
        Territory {
            guild: GuildRef {
                uuid: format!("{color:?}"),
                name: format!("{color:?}"),
                prefix: "G".into(),
                color: Some(color),
            },
            acquired: chrono::DateTime::UNIX_EPOCH,
            location: Region {
                start: [at.0 - 10, at.1 - 10],
                end: [at.0 + 10, at.1 + 10],
            },
            resources: Default::default(),
            connections: connections.iter().map(|name| name.to_string()).collect(),
            runtime: None,
        }
    }

    /// Two neighbours 100 world units apart along x, listing each other.
    fn pair(left: (u8, u8, u8), right: (u8, u8, u8)) -> ClientTerritoryMap {
        HashMap::from([
            (
                "Left".to_string(),
                ClientTerritory::from_territory("Left", territory((0, 0), left, &["Right"])),
            ),
            (
                "Right".to_string(),
                ClientTerritory::from_territory("Right", territory((100, 0), right, &["Left"])),
            ),
        ])
    }

    fn mesh(
        map: &ClientTerritoryMap,
        scale: f64,
        change: impl FnOnce(&mut RenderSettings),
    ) -> ConnectionMesh {
        let mut settings = settings();
        change(&mut settings);
        let mut mesh = ConnectionMesh::default();
        mesh.rebuild(map, scale, &settings);
        mesh
    }

    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1e-5
    }

    /// Distinct y offsets (CSS px at `scale`) of the line-list vertices.
    fn hairline_offsets(mesh: &ConnectionMesh, scale: f32) -> Vec<f32> {
        mesh.lines
            .chunks(2)
            .map(|pair| pair[0].world[1] * scale)
            .collect()
    }

    #[test]
    fn classic_at_full_thickness_is_the_original_five_hairlines() {
        let map = pair(RED, RED);
        let mesh = mesh(&map, 1.0, |_| {});
        assert!(mesh.triangles.is_empty());
        let mut offsets = hairline_offsets(&mesh, 1.0);
        // The edge runs either way depending on map iteration order, so compare sorted.
        offsets.sort_by(f32::total_cmp);
        let expected = [-1.2, -0.6, 0.0, 0.6, 1.2];
        assert_eq!(offsets.len(), expected.len());
        for (actual, expected) in offsets.iter().zip(expected) {
            assert!(close(*actual, expected), "{offsets:?}");
        }
        let alphas: Vec<f32> = mesh.lines.chunks(2).map(|pair| pair[0].color[3]).collect();
        for (actual, factor) in alphas.iter().zip([0.28, 0.6, 1.0, 0.6, 0.28]) {
            assert!(close(*actual, 0.16 * factor), "{alphas:?}");
        }
        assert!(mesh.lines.iter().all(|v| v.color[..3] == [1.0, 1.0, 1.0]));
    }

    #[test]
    fn classic_thickness_widens_the_band_without_opening_gaps() {
        let map = pair(RED, RED);
        for thickness in [0.7, 1.0, 1.35, 2.5] {
            let mesh = mesh(&map, 1.0, |s| s.connection_thickness_scale = thickness);
            let mut offsets = hairline_offsets(&mesh, 1.0);
            offsets.sort_by(f32::total_cmp);
            let outer = offsets.iter().fold(0.0_f32, |m, o| m.max(o.abs()));
            assert!(close(outer, 1.2 * thickness), "{thickness}: {offsets:?}");
            for gap in offsets.windows(2).map(|w| w[1] - w[0]) {
                assert!(gap <= 0.6 + 1e-5, "{thickness}: {offsets:?}");
            }
        }
        // The bold band keeps its own spacing.
        let bold = mesh(&map, 1.0, |s| {
            s.bold_connections = true;
            s.connection_thickness_scale = 2.0;
        });
        let mut offsets = hairline_offsets(&bold, 1.0);
        // The edge runs either way depending on map iteration order, so compare sorted.
        offsets.sort_by(f32::total_cmp);
        assert!(close(offsets[0], -3.2), "{offsets:?}");
        assert!(offsets.windows(2).all(|w| w[1] - w[0] <= 0.8 + 1e-5));
    }

    #[test]
    fn solid_lines_are_one_colour_of_a_true_width() {
        let map = pair(RED, RED);
        for (thickness, bold, width_px) in [
            (1.0, false, 1.5),
            (2.0, false, 3.0),
            (2.0, true, 6.0),
            (0.7, false, 1.05),
        ] {
            let mesh = mesh(&map, 1.0, |s| {
                s.connection_style = ConnectionStyle::White;
                s.connection_thickness_scale = thickness;
                s.bold_connections = bold;
            });
            assert!(mesh.lines.is_empty());
            assert_eq!(mesh.triangles.len(), 6);
            assert!(
                mesh.triangles
                    .iter()
                    .all(|v| v.color == [1.0, 1.0, 1.0, 1.0])
            );
            // The strip runs along the centreline; its width is a screen-space offset.
            assert!(mesh.triangles.iter().all(|v| v.world[1] == 0.0));
            let ys: Vec<f32> = mesh.triangles.iter().map(|v| v.offset[1]).collect();
            let span = ys.iter().fold(f32::MIN, |m, y| m.max(*y))
                - ys.iter().fold(f32::MAX, |m, y| m.min(*y));
            assert!(close(span, width_px), "{thickness} {bold}: {span}");
        }
    }

    #[test]
    fn solid_strips_are_the_same_under_any_projection() {
        // The minimap projects the same vertices at about 0.04, below the main camera's
        // limits: a strip must not depend on the main scale it was built at.
        let map = pair(RED, BLUE);
        let build = |scale| {
            mesh(&map, scale, |s| {
                s.connection_style = ConnectionStyle::Guild;
                s.bold_connections = true;
            })
            .triangles
        };
        let mut reference = build(1.0);
        reference.sort_by(|a, b| {
            a.world
                .partial_cmp(&b.world)
                .unwrap()
                .then(a.offset.partial_cmp(&b.offset).unwrap())
        });
        for scale in [0.04, 0.05, 0.3, 8.0] {
            let mut strips = build(scale);
            strips.sort_by(|a, b| {
                a.world
                    .partial_cmp(&b.world)
                    .unwrap()
                    .then(a.offset.partial_cmp(&b.offset).unwrap())
            });
            assert_eq!(strips, reference, "scale {scale}");
        }
        // Classic hairlines carry no screen offset; their spacing is in world units.
        let classic = mesh(&map, 0.5, |_| {});
        assert!(classic.lines.iter().all(|v| v.offset == [0.0, 0.0]));
    }

    #[test]
    fn solid_opacity_is_absolute_and_never_fades_with_zoom() {
        let map = pair(RED, RED);
        // Zoomed out past the classic fade, classic draws nothing; solid lines stay.
        assert!(mesh(&map, 0.1, |_| {}).lines.is_empty());
        let solid = mesh(&map, 0.1, |s| {
            s.connection_style = ConnectionStyle::White;
            s.connection_solid_opacity = 0.4;
            // The classic multiplier does not touch solid lines.
            s.connection_opacity_scale = 2.5;
        });
        assert!(solid.triangles.iter().all(|v| close(v.color[3], 0.4)));
    }

    #[test]
    fn edges_between_two_guilds_split_at_the_middle_in_each_colour() {
        let map = pair(RED, BLUE);
        let solid = mesh(&map, 1.0, |s| s.connection_style = ConnectionStyle::Guild);
        assert_eq!(solid.triangles.len(), 12);
        let red = [200.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0, 1.0];
        let blue = [30.0 / 255.0, 60.0 / 255.0, 220.0 / 255.0, 1.0];
        for v in &solid.triangles {
            // Left half (x 0..50) is the left territory's, right half the right one's.
            let expected = if v.world[0] < 50.0 || (v.world[0] == 50.0 && v.color == red) {
                red
            } else {
                blue
            };
            assert_eq!(v.color, expected, "{v:?}");
        }
        // Same split for the classic bold tint, and none within one guild.
        let classic = mesh(&map, 1.0, |s| s.bold_connections = true);
        assert!(classic.lines.chunks(2).all(|pair| {
            let (a, b) = (pair[0].world[0], pair[1].world[0]);
            (a.min(b) == 0.0 && a.max(b) == 50.0) || (a.min(b) == 50.0 && a.max(b) == 100.0)
        }));
        assert_eq!(
            mesh(&pair(BLUE, BLUE), 1.0, |s| s.connection_style =
                ConnectionStyle::Guild)
            .triangles
            .len(),
            6
        );
    }

    #[test]
    fn each_edge_is_drawn_once_and_hidden_connections_not_at_all() {
        let map = pair(RED, RED);
        assert_eq!(mesh(&map, 1.0, |_| {}).lines.len(), 10);
        let hidden = mesh(&map, 1.0, |s| s.show_connections = false);
        assert!(hidden.lines.is_empty() && hidden.triangles.is_empty());
    }
}
