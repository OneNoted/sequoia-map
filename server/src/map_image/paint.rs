//! Draws a scene over the terrain layer.

use tiny_skia::{
    ColorU8, FilterQuality, LineCap, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    Transform,
};

use super::assets::Assets;
use super::frame::Frame;
use super::glyphs::{CAP_CELLS, Face, TextStyle, draw_text};
use super::palette::{self, color};
use super::scene::{Guild, MapScene, Plot};

const FILL_ALPHA: u8 = 84;
const BORDER_ALPHA: u8 = 235;
/// Darker guild colours are lifted to this luma for borders so they read on dark terrain.
const MIN_BORDER_LUMA: f32 = 112.0;
const CROWN_ART_PIXELS: u32 = 16;
/// Smallest label cell: two pixels per font cell gives 14-pixel capitals.
const MIN_CELL: u32 = 2;

/// Draws `scene` over the terrain already in `canvas`.
pub(super) fn paint(canvas: &mut Pixmap, scene: &MapScene, frame: Frame, assets: &Assets) {
    let ui = ui_cell(frame);
    let rects: Vec<Option<Rect>> = scene
        .territories()
        .iter()
        .map(|plot| {
            let [left, top, right, bottom] = plot.bounds;
            frame.rect(left, top, right, bottom)
        })
        .collect();

    let guilds = scene.guilds();
    for (plot, rect) in scene.territories().iter().zip(&rects) {
        if let (Some(rect), Some(guild)) = (rect, owner(guilds, plot)) {
            let [r, g, b] = guild.color;
            fill_rect(canvas, *rect, ColorU8::from_rgba(r, g, b, FILL_ALPHA));
        }
    }
    draw_connections(canvas, scene, frame, ui);
    let border = border_width(ui);
    for (plot, rect) in scene.territories().iter().zip(&rects) {
        let Some(rect) = rect else {
            continue;
        };
        let outline =
            owner(guilds, plot).map_or(palette::UNCLAIMED, |guild| border_color(guild.color));
        stroke_inset(canvas, *rect, outline, border, 0.0);
        if plot.headquarters {
            stroke_inset(canvas, *rect, palette::GOLD, border, border);
        }
    }
    for (plot, rect) in scene.territories().iter().zip(&rects) {
        if let (Some(rect), Some(guild)) = (rect, owner(guilds, plot)) {
            label_territory(canvas, assets, plot, &guild.prefix, *rect, ui);
        }
    }
    if let Some(area) = frame.title_area() {
        draw_title(canvas, assets, scene, area, ui);
    }
    if let Some(area) = frame.legend_area() {
        draw_legend(canvas, assets, guilds, area, ui);
    }
}

fn owner<'a>(guilds: &'a [Guild], plot: &Plot) -> Option<&'a Guild> {
    plot.owner.and_then(|index| guilds.get(index))
}

/// Pixels per font cell for the legend, subtitle and largest territory labels; it grows
/// with the export so text keeps its size relative to the map.
fn ui_cell(frame: Frame) -> u32 {
    ((frame.units() + 24) / 72).clamp(MIN_CELL, 6)
}

fn border_width(ui: u32) -> f32 {
    (ui as f32 * 0.5).round().max(2.0)
}

fn border_color([r, g, b]: [u8; 3]) -> ColorU8 {
    let [rf, gf, bf] = [r, g, b].map(f32::from);
    let luma = 0.2126 * rf + 0.7152 * gf + 0.0722 * bf;
    let lift = ((MIN_BORDER_LUMA - luma) / (255.0 - luma)).clamp(0.0, 1.0);
    let [r, g, b] = [rf, gf, bf].map(|channel| (channel + (255.0 - channel) * lift).round() as u8);
    ColorU8::from_rgba(r, g, b, BORDER_ALPHA)
}

fn solid_paint(value: ColorU8) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(color(value));
    paint
}

fn fill_rect(canvas: &mut Pixmap, rect: Rect, value: ColorU8) {
    canvas.fill_rect(rect, &solid_paint(value), Transform::identity(), None);
}

/// Strokes the rectangle's inside edge, `inset` pixels in from its bounds.
fn stroke_inset(canvas: &mut Pixmap, rect: Rect, value: ColorU8, width: f32, inset: f32) {
    let half = width / 2.0 + inset;
    let Some(inner) = Rect::from_ltrb(
        rect.left() + half,
        rect.top() + half,
        rect.right() - half,
        rect.bottom() - half,
    ) else {
        return;
    };
    let path = PathBuilder::from_rect(inner);
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    canvas.stroke_path(
        &path,
        &solid_paint(value),
        &stroke,
        Transform::identity(),
        None,
    );
}

fn draw_connections(canvas: &mut Pixmap, scene: &MapScene, frame: Frame, ui: u32) {
    let mut builder = PathBuilder::new();
    for &[x1, z1, x2, z2] in scene.connections() {
        let (from_x, from_y) = frame.point(x1 as f32 + 0.5, z1 as f32 + 0.5);
        let (to_x, to_y) = frame.point(x2 as f32 + 0.5, z2 as f32 + 0.5);
        builder.move_to(from_x, from_y);
        builder.line_to(to_x, to_y);
    }
    let Some(path) = builder.finish() else {
        return;
    };
    let stroke = Stroke {
        width: (ui as f32 * 0.45).max(1.5),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    canvas.stroke_path(
        &path,
        &solid_paint(palette::CONNECTION),
        &stroke,
        Transform::identity(),
        None,
    );
}

/// One way to label a territory: its guild tag, optionally with the hold time below and
/// the headquarters crown above.
#[derive(Debug, Clone, Copy)]
struct LabelPlan {
    tag: TextStyle,
    held: Option<TextStyle>,
    crown: Option<u32>,
}

impl LabelPlan {
    /// Vertical gap between the plan's parts: one font cell of the tag.
    fn gap(self) -> f32 {
        self.tag.cell as f32
    }

    /// Width and height of the stacked parts, including the text halo.
    fn size(self, assets: &Assets, tag: &str, held: &str) -> (f32, f32) {
        let halo = 2.0 * self.tag.halo_width();
        let mut width = assets.bold.width(tag, self.tag.cell) + halo;
        let mut height = self.tag.cap_height() + halo;
        if let Some(style) = self.held {
            width = width.max(assets.regular.width(held, style.cell) + halo);
            height += self.gap() + style.cap_height();
        }
        if let Some(size) = self.crown {
            width = width.max(size as f32);
            height += self.gap() + size as f32;
        }
        (width, height)
    }
}

fn label_style(cell: u32, fill: ColorU8) -> TextStyle {
    TextStyle {
        cell,
        fill: color(fill),
        halo: color(palette::HALO),
    }
}

fn label_territory(
    canvas: &mut Pixmap,
    assets: &Assets,
    plot: &Plot,
    tag: &str,
    rect: Rect,
    max_cell: u32,
) {
    if tag.is_empty() {
        return;
    }
    let Some(plan) = plan_label(assets, plot, tag, rect, max_cell) else {
        return;
    };
    let (_, height) = plan.size(assets, tag, &plot.held);
    let centre_x = rect.left() + rect.width() / 2.0;
    let mut top = (rect.top() + (rect.height() - height) / 2.0 + plan.tag.halo_width()).round();
    if let Some(size) = plan.crown {
        let left = centre_x - size as f32 / 2.0;
        draw_crown(canvas, &assets.crown, (left, top), size);
        top += size as f32 + plan.gap();
    }
    let width = assets.bold.width(tag, plan.tag.cell);
    draw_text(
        canvas,
        &assets.bold,
        tag,
        (centre_x - width / 2.0, top),
        plan.tag,
    );
    if let Some(style) = plan.held {
        top += plan.tag.cap_height() + plan.gap();
        let width = assets.regular.width(&plot.held, style.cell);
        let left = centre_x - width / 2.0;
        draw_text(canvas, &assets.regular, &plot.held, (left, top), style);
    }
}

/// The richest label that fits inside the territory with a pixel to spare: a hold time is
/// preferred over larger text, and the headquarters crown over neither.
fn plan_label(
    assets: &Assets,
    plot: &Plot,
    tag: &str,
    rect: Rect,
    max_cell: u32,
) -> Option<LabelPlan> {
    // About 1.4 times the tag's cap height, at a whole number of pixels per art pixel.
    let crown_size = |cell: u32| {
        let art_scale = (14 * CAP_CELLS * cell + 5 * CROWN_ART_PIXELS) / (10 * CROWN_ART_PIXELS);
        CROWN_ART_PIXELS * art_scale.max(1)
    };
    let variants = [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .filter(|&(_, crown)| plot.headquarters || !crown);
    variants
        .flat_map(|(held, crown)| {
            (MIN_CELL..=max_cell.max(MIN_CELL))
                .rev()
                .map(move |cell| LabelPlan {
                    tag: label_style(cell, palette::TEXT),
                    held: held
                        .then(|| label_style(cell.saturating_sub(1).max(MIN_CELL), palette::HELD)),
                    crown: crown.then(|| crown_size(cell)),
                })
        })
        .find(|plan| {
            let (width, height) = plan.size(assets, tag, &plot.held);
            width <= rect.width() - 2.0 && height <= rect.height() - 2.0
        })
}

fn draw_crown(canvas: &mut Pixmap, crown: &Pixmap, (x, y): (f32, f32), size: u32) {
    let scale = size as f32 / crown.width() as f32;
    canvas.draw_pixmap(
        0,
        0,
        crown.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Nearest,
            ..PixmapPaint::default()
        },
        Transform::from_row(scale, 0.0, 0.0, scale, x.round(), y.round()),
        None,
    );
}

fn draw_title(canvas: &mut Pixmap, assets: &Assets, scene: &MapScene, area: Rect, ui: u32) {
    let lines = [
        (
            &assets.bold,
            "Territory Map".to_string(),
            ui + 2,
            palette::GOLD,
        ),
        (
            &assets.regular,
            scene.as_of().format("%-d %b %Y, %H:%M UTC").to_string(),
            ui,
            palette::TEXT,
        ),
        (
            &assets.regular,
            format!(
                "{} territories \u{b7} {} guilds",
                scene.territory_count(),
                scene.guilds().len()
            ),
            ui,
            palette::MUTED,
        ),
    ];
    let mut top = area.top();
    for (face, text, preferred, fill) in lines {
        let cell = (MIN_CELL..=preferred)
            .rev()
            .find(|cell| face.width(&text, *cell) <= area.width())
            .unwrap_or(MIN_CELL);
        let style = label_style(cell, fill);
        draw_text(canvas, face, &text, (area.left(), top), style);
        top += style.cap_height() + 4.0 * ui as f32;
    }
}

/// Widths of one legend column's parts; each column is only as wide as its own entries.
#[derive(Debug, Clone, Copy, Default)]
struct LegendColumn {
    tag: f32,
    name: f32,
    count: f32,
    /// Width of a "+N more" summary in this column, if it holds one.
    summary: f32,
}

/// The guild legend at one text size.
#[derive(Debug, Clone)]
struct LegendLayout {
    cell: u32,
    rows: usize,
    /// Guilds listed; any others are summarised in one final "+N more" slot.
    shown: usize,
    columns: Vec<LegendColumn>,
}

const MAX_LEGEND_COLUMNS: usize = 4;

impl LegendLayout {
    fn new(assets: &Assets, guilds: &[Guild], cell: u32, shown: usize, columns: usize) -> Self {
        let summary = (shown < guilds.len()).then(|| {
            assets
                .regular
                .width(&more_guilds(guilds.len() - shown), cell)
        });
        let rows = (shown + usize::from(summary.is_some())).div_ceil(columns);
        let mut widths: Vec<LegendColumn> = guilds[..shown]
            .chunks(rows)
            .map(|chunk| LegendColumn {
                tag: widest(&assets.bold, cell, chunk.iter().map(|guild| &guild.prefix)),
                name: widest(&assets.regular, cell, chunk.iter().map(|guild| &guild.name)),
                count: widest(
                    &assets.regular,
                    cell,
                    chunk.iter().map(|guild| guild.territories.to_string()),
                ),
                summary: 0.0,
            })
            .collect();
        if let Some(width) = summary {
            if shown % rows == 0 {
                widths.push(LegendColumn::default());
            }
            if let Some(last) = widths.last_mut() {
                last.summary = width;
            }
        }
        Self {
            cell,
            rows,
            shown,
            columns: widths,
        }
    }

    fn swatch(&self) -> f32 {
        (7 * self.cell) as f32
    }

    fn tag_offset(&self) -> f32 {
        self.swatch() + (3 * self.cell) as f32
    }

    fn name_offset(&self, column: LegendColumn) -> f32 {
        self.tag_offset() + column.tag + (5 * self.cell) as f32
    }

    fn column_width(&self, column: LegendColumn) -> f32 {
        let entries =
            self.name_offset(column) + column.name + (5 * self.cell) as f32 + column.count;
        entries.max(column.summary)
    }

    fn column_gap(&self) -> f32 {
        (8 * self.cell) as f32
    }

    fn row_height(&self) -> f32 {
        (11 * self.cell) as f32
    }

    fn width(&self) -> f32 {
        self.columns
            .iter()
            .map(|column| self.column_width(*column))
            .sum::<f32>()
            + self.columns.len().saturating_sub(1) as f32 * self.column_gap()
    }

    fn height(&self) -> f32 {
        self.rows as f32 * self.row_height() - (4 * self.cell) as f32
    }

    /// Left edge of each column when the legend's right edge is at `right`.
    fn column_lefts(&self, right: f32) -> Vec<f32> {
        let mut left = right - self.width();
        self.columns
            .iter()
            .map(|column| {
                let this = left;
                left += self.column_width(*column) + self.column_gap();
                this.round()
            })
            .collect()
    }
}

fn more_guilds(hidden: usize) -> String {
    format!("+{hidden} more guilds")
}

fn widest<T: AsRef<str>>(face: &Face, cell: u32, texts: impl Iterator<Item = T>) -> f32 {
    texts
        .map(|text| face.width(text.as_ref(), cell))
        .fold(0.0, f32::max)
}

/// Largest legend that fits `area`: every guild if possible, else as many as fit at the
/// smallest size.
fn legend_layout(
    assets: &Assets,
    guilds: &[Guild],
    area: Rect,
    max_cell: u32,
) -> Option<LegendLayout> {
    let fit = |cell: u32, shown: usize| {
        (1..=MAX_LEGEND_COLUMNS)
            .map(|columns| LegendLayout::new(assets, guilds, cell, shown, columns))
            .find(|layout| layout.width() <= area.width() && layout.height() <= area.height())
    };
    (MIN_CELL..=max_cell.max(MIN_CELL))
        .rev()
        .find_map(|cell| fit(cell, guilds.len()))
        .or_else(|| {
            (1..guilds.len())
                .rev()
                .find_map(|shown| fit(MIN_CELL, shown))
        })
}

fn draw_legend(canvas: &mut Pixmap, assets: &Assets, guilds: &[Guild], area: Rect, cell: u32) {
    if guilds.is_empty() {
        return;
    }
    let Some(layout) = legend_layout(assets, guilds, area, cell) else {
        return;
    };
    let lefts = layout.column_lefts(area.right());
    let text = label_style(layout.cell, palette::TEXT);
    let muted = label_style(layout.cell, palette::MUTED);
    let gold = label_style(layout.cell, palette::GOLD);
    let slot = |index: usize| {
        let column = index / layout.rows;
        let y = (area.top() + (index % layout.rows) as f32 * layout.row_height()).round();
        (column, lefts.get(column).copied().unwrap_or(area.left()), y)
    };
    for (index, guild) in guilds[..layout.shown].iter().enumerate() {
        let (column, x, y) = slot(index);
        let widths = layout.columns.get(column).copied().unwrap_or_default();
        let swatch = layout.swatch();
        if let Some(outline) = Rect::from_xywh(x, y, swatch, swatch) {
            fill_rect(canvas, outline, palette::HALO);
        }
        if let Some(inner) = Rect::from_xywh(x + 2.0, y + 2.0, swatch - 4.0, swatch - 4.0) {
            let [r, g, b] = guild.color;
            fill_rect(canvas, inner, ColorU8::from_rgba(r, g, b, 255));
        }
        let prefix = &guild.prefix;
        draw_text(
            canvas,
            &assets.bold,
            prefix,
            (x + layout.tag_offset(), y),
            text,
        );
        let name_x = x + layout.name_offset(widths);
        draw_text(canvas, &assets.regular, &guild.name, (name_x, y), muted);
        let territories = guild.territories.to_string();
        let count_x =
            x + layout.column_width(widths) - assets.regular.width(&territories, layout.cell);
        draw_text(canvas, &assets.regular, &territories, (count_x, y), gold);
    }
    if layout.shown < guilds.len() {
        let (_, x, y) = slot(layout.shown);
        let summary = more_guilds(guilds.len() - layout.shown);
        draw_text(canvas, &assets.regular, &summary, (x, y), muted);
    }
}
