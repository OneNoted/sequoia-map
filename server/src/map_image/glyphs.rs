//! Vector text in the map's bundled pixel font.

use tiny_skia::{Color, FillRule, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// The Minecraft faces draw on a 100-unit grid, ten grid cells per em. Rendering at a whole
/// number of pixels per cell, from a whole-pixel origin, keeps every stem on the pixel grid.
const UNITS_PER_CELL: f32 = 100.0;
/// Cap height in cells; labels are laid out by caps because they rarely have descenders.
pub(super) const CAP_CELLS: u32 = 7;

/// Printable ASCII and Latin-1, the range the bundled faces cover.
const GLYPH_RANGE: std::ops::RangeInclusive<char> = ' '..='\u{ff}';

/// A face reduced to the outlines and advances the renderer needs.
pub(super) struct Face {
    glyphs: Vec<Option<Glyph>>,
    fallback: Glyph,
}

#[derive(Clone)]
struct Glyph {
    outline: Option<Path>,
    advance: f32,
    ink_right: f32,
}

#[derive(Debug, thiserror::Error)]
#[error("font has no usable glyph for '?'")]
pub(super) struct FaceError;

impl Face {
    /// Extracts outlines from OpenType font bytes.
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] when the font cannot be parsed or lacks the `?` fallback glyph.
    pub(super) fn parse(font: &[u8]) -> Result<Self, FaceError> {
        let face = ttf_parser::Face::parse(font, 0).map_err(|_| FaceError)?;
        let glyphs: Vec<Option<Glyph>> = GLYPH_RANGE
            .map(|character| {
                let id = face.glyph_index(character)?;
                let mut outline = OutlinePath(PathBuilder::new());
                let ink_right = face
                    .outline_glyph(id, &mut outline)
                    .map_or(0.0, |bounds| f32::from(bounds.x_max));
                Some(Glyph {
                    outline: outline.0.finish(),
                    advance: f32::from(face.glyph_hor_advance(id)?),
                    ink_right,
                })
            })
            .collect();
        let fallback = glyph_at(&glyphs, '?').cloned().ok_or(FaceError)?;
        Ok(Self { glyphs, fallback })
    }

    fn glyph_or_fallback(&self, character: char) -> &Glyph {
        glyph_at(&self.glyphs, character).unwrap_or(&self.fallback)
    }

    /// Inked width of `text` at `cell` pixels per font cell, excluding the trailing gap.
    pub(super) fn width(&self, text: &str, cell: u32) -> f32 {
        let mut advance = 0.0;
        let mut last_gap = 0.0;
        for character in text.chars() {
            let glyph = self.glyph_or_fallback(character);
            advance += glyph.advance;
            last_gap = glyph.advance - glyph.ink_right;
        }
        (advance - last_gap) * cell as f32 / UNITS_PER_CELL
    }

    /// Outline of `text` with its left edge at `x` and baseline at `baseline`.
    fn path(&self, text: &str, cell: u32, x: f32, baseline: f32) -> Option<Path> {
        let scale = cell as f32 / UNITS_PER_CELL;
        let mut builder = PathBuilder::new();
        let mut pen = 0.0;
        for character in text.chars() {
            let glyph = self.glyph_or_fallback(character);
            if let Some(outline) = &glyph.outline {
                let placed = outline.clone().transform(Transform::from_row(
                    scale,
                    0.0,
                    0.0,
                    -scale,
                    x + pen * scale,
                    baseline,
                ))?;
                builder.push_path(&placed);
            }
            pen += glyph.advance;
        }
        builder.finish()
    }
}

/// How a run of text is painted: a fill over a pixel-grid halo.
#[derive(Debug, Clone, Copy)]
pub(super) struct TextStyle {
    pub(super) cell: u32,
    pub(super) fill: Color,
    pub(super) halo: Color,
}

impl TextStyle {
    pub(super) fn cap_height(self) -> f32 {
        (CAP_CELLS * self.cell) as f32
    }

    /// Distance the halo extends past the glyphs: two pixels for small text, three for large.
    pub(super) fn halo_width(self) -> f32 {
        (self.cell / 2 + 1).clamp(2, 3) as f32
    }
}

/// Draws `text` with its left edge at `x` and cap top at `cap_top`, snapped to whole pixels.
pub(super) fn draw_text(
    canvas: &mut Pixmap,
    face: &Face,
    text: &str,
    (x, cap_top): (f32, f32),
    style: TextStyle,
) {
    let baseline = (cap_top + style.cap_height()).round();
    let Some(path) = face.path(text, style.cell, x.round(), baseline) else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(style.halo);
    let halo = Stroke {
        width: style.halo_width() * 2.0,
        line_join: LineJoin::Miter,
        ..Stroke::default()
    };
    canvas.stroke_path(&path, &paint, &halo, Transform::identity(), None);
    paint.set_color(style.fill);
    canvas.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn glyph_at(glyphs: &[Option<Glyph>], character: char) -> Option<&Glyph> {
    let index = u32::from(character).checked_sub(u32::from(*GLYPH_RANGE.start()))?;
    glyphs.get(usize::try_from(index).ok()?)?.as_ref()
}

struct OutlinePath(PathBuilder);

impl ttf_parser::OutlineBuilder for OutlinePath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}
