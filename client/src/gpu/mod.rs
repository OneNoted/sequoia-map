use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};
use wgpu::util::DeviceExt;

use sequoia_shared::TreasuryLevel;
use sequoia_shared::colors::hsl_to_rgb;

use crate::app::NameColor;
use crate::colors::brighten;
use crate::heat::heat_color_for_count;
use crate::icons::{ICON_COUNT, ResourceAtlas};
use crate::label_layout::{
    IconKind, abbreviate_name, compute_label_layout_metrics, cooldown_color,
    dynamic_label_next_update_age, dynamic_text_state, resource_icon_sequence, write_age,
    write_age_compound,
};
use crate::overlay_sizing::{
    STATIC_NAME_BASELINE_GAP_MULTIPLIER, compute_dynamic_label_sizing,
    compute_resource_icon_size_world, compute_static_label_sizing,
    compute_territory_ornament_sizing, compute_territory_ornament_tint, static_name_bottom_bound,
};
use crate::renderer::{FrameMetrics, InvalidationReason, RenderCapabilities, SceneSnapshot};
use crate::territory::ClientTerritoryMap;
use crate::tiles::{LoadedTile, TileQuality};
use crate::time_format::write_hms;
use crate::viewport::Viewport;

pub type RenderFrameInput<'a> = SceneSnapshot<'a>;

// --- GPU data types ---

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
}

const QUAD_VERTICES: &[Vertex] = &[
    Vertex {
        position: [0.0, 0.0],
    },
    Vertex {
        position: [1.0, 0.0],
    },
    Vertex {
        position: [0.0, 1.0],
    },
    Vertex {
        position: [1.0, 1.0],
    },
];

const QUAD_INDICES: &[u16] = &[0, 1, 2, 2, 1, 3];
const QUAD_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
    offset: 0,
    shader_location: 0,
    format: wgpu::VertexFormat::Float32x2,
}];

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewportUniform {
    offset: [f32; 2],
    scale: f32,
    time: f32,
    resolution: [f32; 2],
    _pad1: [f32; 2],
}

/// Per-territory instance data: 28 floats = 112 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerritoryInstance {
    pub rect: [f32; 4],          // x, y, width, height (world coords)
    pub color: [f32; 4],         // r, g, b, 1.0 — target/static guild color
    pub state: [f32; 4],         // fill_alpha, border_alpha, flags, 0.0
    pub cooldown: [f32; 4],      // acquired_time_rel_secs, unused, unused, unused
    pub anim_color: [f32; 4],    // from_r, from_g, from_b, 0.0
    pub anim_time: [f32; 4],     // start_time_relative_secs, duration_secs, 0, 0
    pub resource_data: [f32; 4], // mode, idx_a, idx_b, flags
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GlowUniform {
    rect: [f32; 4],
    glow_color: [f32; 4],
    expand: f32,
    falloff: f32,
    ring_width: f32,
    fill_tint_alpha: f32,
    fill_tint_rgb: [f32; 3],
    _pad: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TileRectUniform {
    rect: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TextInstance {
    rect: [f32; 4],    // world x, y, w, h
    uv_rect: [f32; 4], // u0, v0, u1, v1
    color: [f32; 4],   // rgba
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct IconInstance {
    rect: [f32; 4],    // world x, y, w, h
    uv_rect: [f32; 4], // u0, v0, u1, v1
    tint: [f32; 4],    // rgba
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ConnectionVertex {
    world_pos: [f32; 2],
    color: [f32; 4],
}

#[derive(Clone, Copy)]
struct GlyphMeta {
    uv_rect: [f32; 4],
    advance: f32,
    draw_offset_x: f32,
    draw_width: f32,
    draw_offset_y: f32,
    draw_height: f32,
}

struct GpuTextRenderer {
    pipeline: wgpu::RenderPipeline,
    fill_bind_group: wgpu::BindGroup,
    halo_bind_group: wgpu::BindGroup,
    static_fill_buffer: wgpu::Buffer,
    static_fill_count: u32,
    static_fill_capacity: u32,
    static_fill_instances: Vec<TextInstance>,
    static_halo_buffer: wgpu::Buffer,
    static_halo_count: u32,
    static_halo_capacity: u32,
    static_halo_instances: Vec<TextInstance>,
    dynamic_fill_buffer: wgpu::Buffer,
    dynamic_fill_count: u32,
    dynamic_fill_capacity: u32,
    dynamic_fill_instances: Vec<TextInstance>,
    dynamic_halo_buffer: wgpu::Buffer,
    dynamic_halo_count: u32,
    dynamic_halo_capacity: u32,
    dynamic_halo_instances: Vec<TextInstance>,
    glyphs: HashMap<char, GlyphMeta>,
    kerning: HashMap<u32, f32>,
    line_height: f32,
}

struct GpuIconRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_count: u32,
    instance_capacity: u32,
    instances_buf: Vec<IconInstance>,
    uv_by_kind: HashMap<IconKind, [f32; 4]>,
    ornament_aspect: f32,
}

const GLYPH_ATLAS_FONT_PX: f64 = 96.0;
const GLYPH_ATLAS_PADDING_PX: f64 = 6.0;
const GLYPH_ATLAS_STROKE_FACTOR: f64 = 0.094;
const GLYPH_ATLAS_STROKE_MIN_PX: f64 = 1.6;
const GLYPH_ATLAS_BLEED_FACTOR: f32 = 0.62;
const GLYPH_ATLAS_BLEED_EXTRA_PX: f32 = 1.1;
const GLYPH_ATLAS_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 [](){}<>+-=_,.:;!?'/\\\\|@#$%^&*~`\\\"…";
const GLYPH_ATLAS_COLS: usize = 16;
const CONNECTION_LINE_STEPS_NORMAL: &[(f32, f32)] = &[
    (-1.2, 0.28),
    (-0.6, 0.6),
    (0.0, 1.0),
    (0.6, 0.6),
    (1.2, 0.28),
];
const CONNECTION_LINE_STEPS_BOLD: &[(f32, f32)] = &[
    (-1.6, 0.45),
    (-0.8, 0.75),
    (0.0, 1.0),
    (0.8, 0.75),
    (1.6, 0.45),
];
const MINIMAP_W: f32 = 200.0;
const MINIMAP_H: f32 = 280.0;
const MINIMAP_MARGIN: f32 = 16.0;
const MINIMAP_HISTORY_BOTTOM: f32 = 68.0;
const MINIMAP_DEFAULT_WORLD_BOUNDS: (f64, f64, f64, f64) = (-2200.0, -6600.0, 1600.0, 400.0);
const STATIC_TAG_LETTER_SPACING_EM: f32 = 0.07;
const STATIC_NAME_LETTER_SPACING_EM: f32 = 0.057;
const STATIC_TAG_MIN_WIDTH_WORLD: f32 = 88.0;
const STATIC_NAME_MIN_WIDTH_WORLD: f32 = 176.0;
const DYNAMIC_TIME_LETTER_SPACING_EM: f32 = 0.035;
const DYNAMIC_COOLDOWN_LETTER_SPACING_EM_MIN: f32 = 0.0035;
/// Minimum viewport scale required before per-territory timer text (held/cooldown) is shown.
/// Keeps the default world view uncluttered; timers appear when users zoom in.
const DYNAMIC_TIMER_VISIBILITY_MIN_SCALE: f64 = 0.31;
/// Minimum viewport scale at which any territory labels are drawn.
/// Below this zoom level all text (static tags, dynamic time, icons) is hidden uniformly.
const LABEL_VISIBILITY_MIN_SCALE: f64 = 0.10;
const HQ_CROWN_SIZE_MULTIPLIER: f32 = 1.02;
const HQ_CROWN_MAX_BOX_FRACTION: f32 = 0.40;
const ORNAMENT_MIN_BOX_PX: f32 = 34.0;

#[inline]
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[inline]
fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn glyph_mip_level_count(width: u32, height: u32) -> u32 {
    if width == 0 || height == 0 {
        return 0;
    }
    u32::BITS - width.max(height).leading_zeros()
}

#[inline]
fn downsample_rgba8_level(src: &[u8], src_w: u32, src_h: u32) -> (u32, u32, Vec<u8>) {
    let next_w = (src_w / 2).max(1);
    let next_h = (src_h / 2).max(1);
    let mut dst = vec![0u8; (next_w as usize) * (next_h as usize) * 4];
    for y in 0..next_h {
        for x in 0..next_w {
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            let src_x0 = x * 2;
            let src_y0 = y * 2;
            for oy in 0..2 {
                let sy = src_y0 + oy;
                if sy >= src_h {
                    continue;
                }
                for ox in 0..2 {
                    let sx = src_x0 + ox;
                    if sx >= src_w {
                        continue;
                    }
                    let si = ((sy * src_w + sx) * 4) as usize;
                    sum[0] += src[si] as u32;
                    sum[1] += src[si + 1] as u32;
                    sum[2] += src[si + 2] as u32;
                    sum[3] += src[si + 3] as u32;
                    count += 1;
                }
            }
            let di = ((y * next_w + x) * 4) as usize;
            dst[di] = ((sum[0] + count / 2) / count) as u8;
            dst[di + 1] = ((sum[1] + count / 2) / count) as u8;
            dst[di + 2] = ((sum[2] + count / 2) / count) as u8;
            dst[di + 3] = ((sum[3] + count / 2) / count) as u8;
        }
    }
    (next_w, next_h, dst)
}

#[inline]
fn write_texture_mip_chain(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    base_pixels: &[u8],
    base_w: u32,
    base_h: u32,
) {
    let mip_levels = glyph_mip_level_count(base_w, base_h);
    if mip_levels == 0 {
        return;
    }

    let write_level = |mip_level: u32, pixels: &[u8], width: u32, height: u32| {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    };

    write_level(0, base_pixels, base_w, base_h);

    if mip_levels == 1 {
        return;
    }

    let mut src_w = base_w;
    let mut src_h = base_h;
    let mut src_pixels = base_pixels.to_vec();
    for level in 1..mip_levels {
        let (next_w, next_h, next_pixels) = downsample_rgba8_level(&src_pixels, src_w, src_h);
        write_level(level, &next_pixels, next_w, next_h);
        src_pixels = next_pixels;
        src_w = next_w;
        src_h = next_h;
    }
}

fn name_color_rgba(name_color: NameColor, guild_rgb: (u8, u8, u8)) -> [f32; 4] {
    match name_color {
        NameColor::White => [220.0 / 255.0, 218.0 / 255.0, 210.0 / 255.0, 0.95],
        NameColor::Guild => {
            let (r, g, b) = brighten(guild_rgb.0, guild_rgb.1, guild_rgb.2, 1.6);
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.95]
        }
        NameColor::Gold => [245.0 / 255.0, 197.0 / 255.0, 66.0 / 255.0, 0.95],
        NameColor::Copper => [181.0 / 255.0, 103.0 / 255.0, 39.0 / 255.0, 0.95],
        NameColor::Muted => [120.0 / 255.0, 116.0 / 255.0, 112.0 / 255.0, 0.86],
    }
}

#[inline]
fn kerning_key(prev: char, next: char) -> u32 {
    ((prev as u32) << 16) | (next as u32)
}

fn gpu_console_diag_enabled() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::get(
        window.as_ref(),
        &wasm_bindgen::JsValue::from_str("__SEQUOIA_GPU_DIAG__"),
    )
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false)
}

fn gpu_is_firefox() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .map(|ua| {
            let ua = ua.to_ascii_lowercase();
            ua.contains("firefox") || ua.contains("fxios")
        })
        .unwrap_or(false)
}

fn get_2d_context(
    canvas: &HtmlCanvasElement,
    will_read_frequently: bool,
) -> Option<CanvasRenderingContext2d> {
    if will_read_frequently {
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            options.as_ref(),
            &wasm_bindgen::JsValue::from_str("willReadFrequently"),
            &wasm_bindgen::JsValue::from_bool(true),
        );
        if let Ok(Some(ctx)) = canvas.get_context_with_context_options("2d", options.as_ref())
            && let Ok(ctx2d) = ctx.dyn_into::<CanvasRenderingContext2d>()
        {
            return Some(ctx2d);
        }
    }
    canvas
        .get_context("2d")
        .ok()
        .flatten()?
        .dyn_into::<CanvasRenderingContext2d>()
        .ok()
}

fn line_units_with_tracking(
    text: &str,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    tracking_units: f32,
) -> f32 {
    let tracking_units = tracking_units.max(0.0);
    let mut units = 0.0f32;
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        let Some(glyph) = glyphs.get(&ch).or_else(|| glyphs.get(&'?')) else {
            continue;
        };
        if let Some(prev_ch) = prev {
            units += tracking_units;
            units += kerning
                .get(&kerning_key(prev_ch, ch))
                .copied()
                .unwrap_or(0.0);
        }
        units += glyph.advance;
        prev = Some(ch);
    }
    units
}

fn line_units(text: &str, glyphs: &HashMap<char, GlyphMeta>, kerning: &HashMap<u32, f32>) -> f32 {
    line_units_with_tracking(text, glyphs, kerning, 0.0)
}

fn fit_text_to_units_with_tracking(
    text: &str,
    max_units: f32,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    tracking_units: f32,
) -> String {
    let tracking_units = tracking_units.max(0.0);
    if max_units <= 0.0
        || line_units_with_tracking(text, glyphs, kerning, tracking_units) <= max_units
    {
        return text.to_string();
    }
    let ellipsis = "...";
    let ellipsis_units = line_units_with_tracking(ellipsis, glyphs, kerning, tracking_units);
    if ellipsis_units >= max_units {
        return ellipsis.to_string();
    }
    let mut out = String::new();
    let mut used = 0.0f32;
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        let Some(next_units) = glyphs
            .get(&ch)
            .or_else(|| glyphs.get(&'?'))
            .map(|g| g.advance)
        else {
            continue;
        };
        let kern = prev
            .and_then(|prev_ch| kerning.get(&kerning_key(prev_ch, ch)).copied())
            .unwrap_or(0.0);
        let tracking = if prev.is_some() { tracking_units } else { 0.0 };
        if used + tracking + kern + next_units + ellipsis_units > max_units {
            break;
        }
        used += tracking + kern + next_units;
        out.push(ch);
        prev = Some(ch);
    }
    if out.is_empty() {
        ellipsis.to_string()
    } else {
        out.push_str(ellipsis);
        out
    }
}

fn fit_text_to_units(
    text: &str,
    max_units: f32,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
) -> String {
    fit_text_to_units_with_tracking(text, max_units, glyphs, kerning, 0.0)
}

fn push_text_line_with_tracking(
    out: &mut Vec<TextInstance>,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    line_height: f32,
    text: &str,
    cx: f32,
    cy: f32,
    font_height_world: f32,
    max_width_world: f32,
    tracking_units: f32,
    mut color: [f32; 4],
) {
    if text.is_empty() || font_height_world <= 0.0 || max_width_world <= 0.0 || line_height <= 0.0 {
        return;
    }

    let tracking_units = tracking_units.max(0.0);
    let units = line_units_with_tracking(text, glyphs, kerning, tracking_units);
    if units <= 0.0 {
        return;
    }
    let mut scale = font_height_world / line_height;
    let width_world = units * scale;
    if width_world > max_width_world {
        scale *= (max_width_world / width_world).clamp(0.2, 1.0);
    }
    let mut cursor_x = cx - (units * scale) / 2.0;
    let line_top_y = cy - font_height_world * 0.5;
    color[3] = color[3].clamp(0.0, 1.0);
    let mut prev: Option<char> = None;

    for ch in text.chars() {
        let Some(glyph) = glyphs.get(&ch).or_else(|| glyphs.get(&'?')) else {
            continue;
        };
        if let Some(prev_ch) = prev {
            cursor_x += tracking_units * scale;
            cursor_x += kerning
                .get(&kerning_key(prev_ch, ch))
                .copied()
                .unwrap_or(0.0)
                * scale;
        }
        let step_world = glyph.advance * scale;
        let w_world = glyph.draw_width * scale;
        let x_world = cursor_x + glyph.draw_offset_x * scale;
        let h_world = glyph.draw_height * scale;
        let y_world = line_top_y + glyph.draw_offset_y * scale;
        if w_world <= 0.0 {
            cursor_x += step_world;
            prev = Some(ch);
            continue;
        }
        out.push(TextInstance {
            rect: [x_world, y_world, w_world, h_world],
            uv_rect: glyph.uv_rect,
            color,
        });
        cursor_x += step_world;
        prev = Some(ch);
    }
}

fn push_text_line(
    out: &mut Vec<TextInstance>,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    line_height: f32,
    text: &str,
    cx: f32,
    cy: f32,
    font_height_world: f32,
    max_width_world: f32,
    color: [f32; 4],
) {
    push_text_line_with_tracking(
        out,
        glyphs,
        kerning,
        line_height,
        text,
        cx,
        cy,
        font_height_world,
        max_width_world,
        0.0,
        color,
    );
}

fn push_text_line_dual_with_tracking(
    fill_out: &mut Vec<TextInstance>,
    halo_out: &mut Vec<TextInstance>,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    line_height: f32,
    text: &str,
    cx: f32,
    cy: f32,
    font_height_world: f32,
    max_width_world: f32,
    tracking_units: f32,
    fill_color: [f32; 4],
    halo_color: [f32; 4],
) {
    push_text_line_with_tracking(
        halo_out,
        glyphs,
        kerning,
        line_height,
        text,
        cx,
        cy,
        font_height_world,
        max_width_world,
        tracking_units,
        halo_color,
    );
    push_text_line_with_tracking(
        fill_out,
        glyphs,
        kerning,
        line_height,
        text,
        cx,
        cy,
        font_height_world,
        max_width_world,
        tracking_units,
        fill_color,
    );
}

fn push_text_line_dual(
    fill_out: &mut Vec<TextInstance>,
    halo_out: &mut Vec<TextInstance>,
    glyphs: &HashMap<char, GlyphMeta>,
    kerning: &HashMap<u32, f32>,
    line_height: f32,
    text: &str,
    cx: f32,
    cy: f32,
    font_height_world: f32,
    max_width_world: f32,
    fill_color: [f32; 4],
    halo_color: [f32; 4],
) {
    push_text_line_dual_with_tracking(
        fill_out,
        halo_out,
        glyphs,
        kerning,
        line_height,
        text,
        cx,
        cy,
        font_height_world,
        max_width_world,
        0.0,
        fill_color,
        halo_color,
    );
}

// --- Tile texture cache ---

struct TileTexture {
    bind_group: wgpu::BindGroup,
    rect: [f32; 4], // [x, z, width, height] in world coords
    quality: TileQuality,
}

// --- GpuRenderer ---

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    // Shared geometry
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,

    // Viewport uniform (shared by all pipelines)
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group_layout: wgpu::BindGroupLayout,
    viewport_bind_group: wgpu::BindGroup,
    minimap_viewport_buffer: wgpu::Buffer,
    minimap_viewport_bind_group: wgpu::BindGroup,

    // Territory fill+border pipeline (instanced)
    territory_pipeline: wgpu::RenderPipeline,
    instance_buffer: wgpu::Buffer,
    instance_count: u32,
    instance_capacity: u32,

    // Glow pipeline (1-2 quads for selection/hover)
    glow_pipeline: wgpu::RenderPipeline,
    glow_buffer_sel: wgpu::Buffer,
    glow_bind_group_sel: wgpu::BindGroup,
    glow_buffer_hov: wgpu::Buffer,
    glow_bind_group_hov: wgpu::BindGroup,

    // Tile pipeline
    tile_pipeline: wgpu::RenderPipeline,
    tile_bind_group_layout: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,
    tile_textures: HashMap<usize, TileTexture>,
    tile_upload_canvas: Option<HtmlCanvasElement>,
    tile_upload_ctx: Option<CanvasRenderingContext2d>,
    tile_upload_canvas_size: (u32, u32),
    tile_world_bounds: Option<(f64, f64, f64, f64)>,

    // Connection line pipeline (full GPU mode only)
    connection_pipeline: wgpu::RenderPipeline,
    connection_fill_pipeline: wgpu::RenderPipeline,
    connection_buffer: wgpu::Buffer,
    connection_count: u32,
    connection_capacity: u32,
    connection_dirty: bool,
    connection_vertices: Vec<ConnectionVertex>,
    connection_drawn_set: HashSet<(u64, u64)>,
    minimap_indicator_buffer: wgpu::Buffer,
    minimap_indicator_capacity: u32,
    minimap_bg_buffer: wgpu::Buffer,

    // Text pipelines (static + dynamic)
    text_renderer: Option<GpuTextRenderer>,
    static_text_dirty: bool,
    dynamic_text_dirty: bool,
    static_zoom_bucket: i32,
    dynamic_zoom_bucket: i32,
    dynamic_reference_time_secs: i64,
    dynamic_next_update_secs: i64,
    dynamic_refresh_deferred: bool,
    dynamic_timer_visible_at_zoom: bool,
    territory_name_cache: HashMap<String, (String, String)>,

    // Resource icon pipeline
    icon_renderer: Option<GpuIconRenderer>,
    icon_dirty: bool,

    // Track current dimensions
    width: u32,
    height: u32,
    dpr: f32,

    // Dirty tracking: skip instance rebuild during pan/zoom
    instance_dirty: bool,

    // Cached max animation end time (epoch ms) — avoids scanning all
    // territories every frame just to check if animations are active.
    max_anim_end_ms: f64,

    // Relative timing: epoch ms at init, for f32-safe shader time
    start_time_ms: f64,

    // Persistent instance buffer to avoid per-rebuild allocation
    instances_buf: Vec<TerritoryInstance>,

    // Diagnostics
    diag_static_rebuilds: u32,
    diag_dynamic_rebuilds: u32,
    diag_icon_rebuilds: u32,
    diag_pan_only_zero_rebuild_frames: u32,
    diag_last_vp: (f64, f64, f64),
    diag_console_logging: bool,
    last_render_time_ms: f64,
    capabilities: RenderCapabilities,
    frame_metrics: FrameMetrics,

    // Settings
    pub thick_cooldown_borders: bool,
    pub resource_highlight: bool,
    pub use_static_gpu_labels: bool,
    pub use_full_gpu_text: bool,
    pub static_show_names: bool,
    pub static_abbreviate_names: bool,
    pub static_name_color: NameColor,
    pub show_connections: bool,
    pub bold_connections: bool,
    pub connection_opacity_scale: f32,
    pub connection_thickness_scale: f32,
    pub static_tag_color: NameColor,
    pub use_readable_font: bool,
    pub dynamic_show_countdown: bool,
    pub dynamic_show_granular_map_time: bool,
    pub dynamic_show_compound_map_time: bool,
    pub dynamic_show_resource_icons: bool,
    pub label_scale_master: f32,
    pub label_scale_static_tag: f32,
    pub label_scale_static_name: f32,
    pub label_scale_dynamic: f32,
    pub label_scale_icons: f32,
}

impl GpuRenderer {
    fn tile_world_bounds(tiles: &[LoadedTile]) -> Option<(f64, f64, f64, f64)> {
        if tiles.is_empty() {
            return None;
        }
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for tile in tiles {
            let x1 = tile.x1.min(tile.x2) as f64;
            let y1 = tile.z1.min(tile.z2) as f64;
            let x2 = tile.x1.max(tile.x2) as f64 + 1.0;
            let y2 = tile.z1.max(tile.z2) as f64 + 1.0;
            min_x = min_x.min(x1);
            min_y = min_y.min(y1);
            max_x = max_x.max(x2);
            max_y = max_y.max(y2);
        }
        Some((min_x, min_y, max_x, max_y))
    }

    #[inline]
    fn quad_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &QUAD_VERTEX_ATTRIBUTES,
        }
    }

    /// Async initialization with a WebGL2-only path.
    pub async fn init(canvas: HtmlCanvasElement) -> Result<Self, String> {
        web_sys::console::log_1(&"wgpu init: using WebGL2 backend (WebGPU disabled)".into());
        Self::init_with_backends(canvas, wgpu::Backends::GL, "webgl").await
    }

    /// Core initialization parameterized by backend selection.
    async fn init_with_backends(
        canvas: HtmlCanvasElement,
        backends: wgpu::Backends,
        backend_path: &str,
    ) -> Result<Self, String> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let rect = canvas.get_bounding_client_rect();
        let css_width = rect.width() as f32;
        let dpr = if css_width > 0.0 {
            (width as f32 / css_width).max(0.5)
        } else {
            web_sys::window()
                .map(|w| w.device_pixel_ratio() as f32)
                .unwrap_or(1.0)
        };

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let surface_target = wgpu::SurfaceTarget::Canvas(canvas);
        let surface = instance
            .create_surface(surface_target)
            .map_err(|e| format!("wgpu init ({backend_path}) create_surface: {e}"))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .ok_or_else(|| format!("wgpu init ({backend_path}): no suitable GPU adapter found"))?;

        // WebGL2 adapters expose zero compute limits, so requesting the plain
        // default limits (which include compute) fails validation.
        let mut required_limits = if backends == wgpu::Backends::GL {
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())
        } else {
            wgpu::Limits::default()
        };
        // Some WebGL2 adapters (for example automation/headless environments) expose
        // lower color-attachment limits than the downlevel default profile. Clamp
        // to adapter-reported capability so init succeeds consistently.
        required_limits.max_color_attachments = required_limits
            .max_color_attachments
            .min(adapter.limits().max_color_attachments);

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("sequoia-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                    ..Default::default()
                },
                None,
            )
            .await
            .map_err(|e| format!("wgpu init ({backend_path}) request_device: {e}"))?;

        let mut surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| format!("wgpu init ({backend_path}): surface unsupported by adapter"))?;
        let caps = surface.get_capabilities(&adapter);

        // Prefer a non-sRGB format so tile textures (uploaded as Rgba8Unorm)
        // pass through without double gamma correction that washes out colors.
        if let Some(format) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            surface_config.format = format;
        }

        // Opaque canvases avoid compositor alpha blending on every frame.
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            surface_config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        } else if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            surface_config.alpha_mode = wgpu::CompositeAlphaMode::PreMultiplied;
        }
        // Firefox tends to pace smoother with lower swapchain queue depth.
        if gpu_is_firefox() {
            surface_config.desired_maximum_frame_latency = 1;
        }
        let format = surface_config.format;

        web_sys::console::log_1(
            &format!(
                "wgpu init: path={backend_path} format={:?} present={:?} alpha={:?} latency={}",
                surface_config.format,
                surface_config.present_mode,
                surface_config.alpha_mode,
                surface_config.desired_maximum_frame_latency,
            )
            .into(),
        );
        surface.configure(&device, &surface_config);

        // --- Shared geometry ---
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-verts"),
            contents: bytemuck::cast_slice(QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-indices"),
            contents: bytemuck::cast_slice(QUAD_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        // --- Viewport uniform ---
        let viewport_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewport-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let viewport_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("viewport-ubo"),
            contents: bytemuck::cast_slice(&[ViewportUniform {
                offset: [0.0, 0.0],
                scale: 1.0,
                time: 0.0,
                resolution: [width as f32 / dpr, height as f32 / dpr],
                _pad1: [0.0, 0.0],
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport-bg"),
            layout: &viewport_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });
        let minimap_viewport_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("minimap-viewport-ubo"),
                contents: bytemuck::cast_slice(&[ViewportUniform {
                    offset: [0.0, 0.0],
                    scale: 1.0,
                    time: 0.0,
                    resolution: [width as f32 / dpr, height as f32 / dpr],
                    _pad1: [0.0, 0.0],
                }]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let minimap_viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("minimap-viewport-bg"),
            layout: &viewport_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: minimap_viewport_buffer.as_entire_binding(),
            }],
        });

        // --- Territory pipeline ---
        let territory_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("territory-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("territory.wgsl").into()),
        });

        let vertex_layout = Self::quad_vertex_layout();

        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TerritoryInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4, // rect
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4, // color
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4, // state
                },
                wgpu::VertexAttribute {
                    offset: 48,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x4, // cooldown
                },
                wgpu::VertexAttribute {
                    offset: 64,
                    shader_location: 5,
                    format: wgpu::VertexFormat::Float32x4, // anim_color
                },
                wgpu::VertexAttribute {
                    offset: 80,
                    shader_location: 6,
                    format: wgpu::VertexFormat::Float32x4, // anim_time
                },
                wgpu::VertexAttribute {
                    offset: 96,
                    shader_location: 7,
                    format: wgpu::VertexFormat::Float32x4, // resource_data
                },
            ],
        };

        let territory_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("territory-pl"),
                bind_group_layouts: &[&viewport_bind_group_layout],
                push_constant_ranges: &[],
            });

        let territory_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("territory-pipeline"),
            layout: Some(&territory_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &territory_shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone(), instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &territory_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let initial_capacity = 256u32;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance-buf"),
            size: (initial_capacity as u64) * std::mem::size_of::<TerritoryInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- Glow pipeline ---
        let glow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glow-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glow.wgsl").into()),
        });

        let glow_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glow-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let glow_buffer_sel = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glow-ubo-sel"),
            size: std::mem::size_of::<GlowUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glow_bind_group_sel = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glow-bg-sel"),
            layout: &glow_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: glow_buffer_sel.as_entire_binding(),
            }],
        });

        let glow_buffer_hov = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glow-ubo-hov"),
            size: std::mem::size_of::<GlowUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glow_bind_group_hov = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glow-bg-hov"),
            layout: &glow_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: glow_buffer_hov.as_entire_binding(),
            }],
        });

        let glow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glow-pl"),
            bind_group_layouts: &[&viewport_bind_group_layout, &glow_bind_group_layout],
            push_constant_ranges: &[],
        });

        let glow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glow-pipeline"),
            layout: Some(&glow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &glow_shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &glow_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- Tile pipeline ---
        let tile_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tile-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile.wgsl").into()),
        });

        let tile_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tile-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let tile_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tile-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let tile_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tile-pl"),
            bind_group_layouts: &[&viewport_bind_group_layout, &tile_bind_group_layout],
            push_constant_ranges: &[],
        });

        let tile_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tile-pipeline"),
            layout: Some(&tile_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &tile_shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &tile_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- Connection pipeline ---
        let connection_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("connection-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("connection.wgsl").into()),
        });
        let connection_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ConnectionVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };
        let connection_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("connection-pl"),
                bind_group_layouts: &[&viewport_bind_group_layout],
                push_constant_ranges: &[],
            });
        let connection_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("connection-pipeline"),
            layout: Some(&connection_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &connection_shader,
                entry_point: Some("vs_main"),
                buffers: &[connection_vertex_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &connection_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let connection_fill_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("connection-fill-pipeline"),
                layout: Some(&connection_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &connection_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ConnectionVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &connection_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        let connection_capacity = 4096u32;
        let connection_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("connection-vertex-buf"),
            size: (connection_capacity as u64) * std::mem::size_of::<ConnectionVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let minimap_indicator_capacity = 16u32;
        let minimap_indicator_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("minimap-indicator-vertex-buf"),
            size: (minimap_indicator_capacity as u64)
                * std::mem::size_of::<ConnectionVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let minimap_bg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("minimap-bg-vertex-buf"),
            size: 6 * std::mem::size_of::<ConnectionVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            vertex_buffer,
            index_buffer,
            viewport_buffer,
            viewport_bind_group_layout,
            viewport_bind_group,
            minimap_viewport_buffer,
            minimap_viewport_bind_group,
            territory_pipeline,
            instance_buffer,
            instance_count: 0,
            instance_capacity: initial_capacity,
            glow_pipeline,
            glow_buffer_sel,
            glow_bind_group_sel,
            glow_buffer_hov,
            glow_bind_group_hov,
            tile_pipeline,
            tile_bind_group_layout,
            tile_sampler,
            tile_textures: HashMap::new(),
            tile_upload_canvas: None,
            tile_upload_ctx: None,
            tile_upload_canvas_size: (0, 0),
            tile_world_bounds: None,
            connection_pipeline,
            connection_fill_pipeline,
            connection_buffer,
            connection_count: 0,
            connection_capacity,
            connection_dirty: true,
            connection_vertices: Vec::new(),
            connection_drawn_set: HashSet::new(),
            minimap_indicator_buffer,
            minimap_indicator_capacity,
            minimap_bg_buffer,
            text_renderer: None,
            static_text_dirty: false,
            dynamic_text_dirty: false,
            static_zoom_bucket: -1,
            dynamic_zoom_bucket: -1,
            dynamic_reference_time_secs: i64::MIN,
            dynamic_next_update_secs: i64::MIN,
            dynamic_refresh_deferred: false,
            dynamic_timer_visible_at_zoom: false,
            territory_name_cache: HashMap::new(),
            icon_renderer: None,
            icon_dirty: false,
            width,
            height,
            dpr,
            instance_dirty: true,
            max_anim_end_ms: 0.0,
            start_time_ms: js_sys::Date::now(),
            instances_buf: Vec::new(),
            diag_static_rebuilds: 0,
            diag_dynamic_rebuilds: 0,
            diag_icon_rebuilds: 0,
            diag_pan_only_zero_rebuild_frames: 0,
            diag_last_vp: (0.0, 0.0, 0.0),
            diag_console_logging: gpu_console_diag_enabled(),
            last_render_time_ms: 0.0,
            capabilities: RenderCapabilities {
                webgl2: backends == wgpu::Backends::GL,
                gpu_text_msdf: true,
                gpu_dynamic_labels: true,
                compatibility_fallback: false,
            },
            frame_metrics: FrameMetrics::default(),
            thick_cooldown_borders: false,
            resource_highlight: false,
            use_static_gpu_labels: false,
            use_full_gpu_text: false,
            static_show_names: false,
            static_abbreviate_names: true,
            static_name_color: NameColor::Guild,
            show_connections: true,
            bold_connections: false,
            connection_opacity_scale: 1.0,
            connection_thickness_scale: 1.0,
            static_tag_color: NameColor::Guild,
            use_readable_font: false,
            dynamic_show_countdown: false,
            dynamic_show_granular_map_time: false,
            dynamic_show_compound_map_time: false,
            dynamic_show_resource_icons: true,
            label_scale_master: 1.0,
            label_scale_static_tag: 1.0,
            label_scale_static_name: 1.0,
            label_scale_dynamic: 1.0,
            label_scale_icons: 1.0,
        };

        if !renderer.ensure_text_renderer() {
            return Err("wgpu init (webgl): failed to initialize GPU text renderer".into());
        }
        renderer.use_static_gpu_labels = true;
        renderer.use_full_gpu_text = true;

        Ok(renderer)
    }

    fn ensure_text_renderer(&mut self) -> bool {
        if self.text_renderer.is_some() {
            return true;
        }
        let vertex_layout = Self::quad_vertex_layout();
        self.text_renderer = Self::init_text_renderer(
            &self.device,
            &self.queue,
            self.surface_config.format,
            &self.viewport_bind_group_layout,
            &vertex_layout,
            self.use_readable_font,
        );
        self.text_renderer.is_some()
    }

    pub fn rebuild_text_renderer(&mut self) {
        self.text_renderer = None;
        if self.ensure_text_renderer() {
            self.static_text_dirty = true;
            self.dynamic_text_dirty = true;
        }
    }

    fn ensure_icon_renderer(&mut self, icons: &ResourceAtlas) -> bool {
        if self.icon_renderer.is_some() {
            return true;
        }
        let vertex_layout = Self::quad_vertex_layout();
        self.icon_renderer = Self::init_icon_renderer(
            &self.device,
            &self.queue,
            self.surface_config.format,
            &self.viewport_bind_group_layout,
            &vertex_layout,
            icons,
        );
        self.icon_renderer.is_some()
    }

    fn init_text_renderer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        viewport_bind_group_layout: &wgpu::BindGroupLayout,
        vertex_layout: &wgpu::VertexBufferLayout<'_>,
        readable_font: bool,
    ) -> Option<GpuTextRenderer> {
        let Some((
            text_bind_group_layout,
            fill_bind_group,
            halo_bind_group,
            glyphs,
            kerning,
            line_height,
        )) = Self::build_glyph_atlas(device, queue, readable_font)
        else {
            web_sys::console::warn_1(
                &"GPU text labels disabled: failed to build dual glyph atlases".into(),
            );
            return None;
        };

        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });

        let text_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TextInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };

        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text-pl"),
            bind_group_layouts: &[viewport_bind_group_layout, &text_bind_group_layout],
            push_constant_ranges: &[],
        });

        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text-pipeline"),
            layout: Some(&text_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone(), text_instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let initial_capacity = 4096u32;
        let make_buffer = |label: &'static str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (initial_capacity as u64) * std::mem::size_of::<TextInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        Some(GpuTextRenderer {
            pipeline: text_pipeline,
            fill_bind_group,
            halo_bind_group,
            static_fill_buffer: make_buffer("text-static-fill-buf"),
            static_fill_count: 0,
            static_fill_capacity: initial_capacity,
            static_fill_instances: Vec::new(),
            static_halo_buffer: make_buffer("text-static-halo-buf"),
            static_halo_count: 0,
            static_halo_capacity: initial_capacity,
            static_halo_instances: Vec::new(),
            dynamic_fill_buffer: make_buffer("text-dynamic-fill-buf"),
            dynamic_fill_count: 0,
            dynamic_fill_capacity: initial_capacity,
            dynamic_fill_instances: Vec::new(),
            dynamic_halo_buffer: make_buffer("text-dynamic-halo-buf"),
            dynamic_halo_count: 0,
            dynamic_halo_capacity: initial_capacity,
            dynamic_halo_instances: Vec::new(),
            glyphs,
            kerning,
            line_height,
        })
    }

    fn init_icon_renderer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        viewport_bind_group_layout: &wgpu::BindGroupLayout,
        vertex_layout: &wgpu::VertexBufferLayout<'_>,
        icons: &ResourceAtlas,
    ) -> Option<GpuIconRenderer> {
        let document = web_sys::window()?.document()?;
        let canvas = document
            .create_element("canvas")
            .ok()?
            .dyn_into::<HtmlCanvasElement>()
            .ok()?;
        let ctx = get_2d_context(&canvas, true)?;
        let resource_atlas_w = icons.resource_image.natural_width().max(1);
        let resource_atlas_h = icons.resource_image.natural_height().max(1);
        let crown_w = icons.hq_crown_image.natural_width().max(1);
        let crown_h = icons.hq_crown_image.natural_height().max(1);
        let ornament_w = icons.territory_ornament_image.natural_width().max(1);
        let ornament_h = icons.territory_ornament_image.natural_height().max(1);
        let icon_cell_w = (resource_atlas_w / ICON_COUNT).max(1);
        let icon_cell_h = resource_atlas_h.max(1);
        let crown_slot_w = icon_cell_w.max(crown_w);
        let resource_x = 0u32;
        let crown_x = resource_x + resource_atlas_w;
        let ornament_x = crown_x + crown_slot_w;
        let atlas_w = ornament_x + ornament_w;
        let atlas_h = icon_cell_h.max(crown_h).max(ornament_h);
        canvas.set_width(atlas_w);
        canvas.set_height(atlas_h);
        ctx.clear_rect(0.0, 0.0, atlas_w as f64, atlas_h as f64);
        ctx.set_image_smoothing_enabled(false);
        ctx.draw_image_with_html_image_element_and_dw_and_dh(
            &icons.resource_image,
            resource_x as f64,
            0.0,
            resource_atlas_w as f64,
            icon_cell_h as f64,
        )
        .ok()?;
        ctx.draw_image_with_html_image_element_and_dw_and_dh(
            &icons.hq_crown_image,
            crown_x as f64,
            0.0,
            crown_slot_w as f64,
            icon_cell_h as f64,
        )
        .ok()?;
        ctx.draw_image_with_html_image_element(
            &icons.territory_ornament_image,
            ornament_x as f64,
            0.0,
        )
        .ok()?;
        ctx.set_image_smoothing_enabled(true);

        let image_data = ctx
            .get_image_data(0.0, 0.0, atlas_w as f64, atlas_h as f64)
            .ok()?;
        let mut pixels = image_data.data().0;
        let mut orn_min_x = ornament_w;
        let mut orn_min_y = ornament_h;
        let mut orn_max_x = 0u32;
        let mut orn_max_y = 0u32;
        let mut orn_found = false;
        for y in 0..ornament_h {
            for x in 0..ornament_w {
                let atlas_px_x = ornament_x + x;
                let idx = ((y * atlas_w + atlas_px_x) * 4) as usize;
                let r = pixels[idx] as u16;
                let g = pixels[idx + 1] as u16;
                let b = pixels[idx + 2] as u16;
                let src_a = pixels[idx + 3] as u16;
                let lum = r.max(g).max(b);
                let alpha = ((lum * src_a + 127) / 255) as u8;
                pixels[idx] = 255;
                pixels[idx + 1] = 255;
                pixels[idx + 2] = 255;
                pixels[idx + 3] = alpha;
                if alpha > 6 {
                    orn_found = true;
                    orn_min_x = orn_min_x.min(x);
                    orn_min_y = orn_min_y.min(y);
                    orn_max_x = orn_max_x.max(x);
                    orn_max_y = orn_max_y.max(y);
                }
            }
        }

        let atlas_wf = atlas_w as f32;
        let atlas_hf = atlas_h as f32;
        let icon_v1 = (icon_cell_h as f32 / atlas_hf).clamp(0.0, 1.0);
        let mut uv_by_kind = HashMap::with_capacity((ICON_COUNT + 2) as usize);
        let resource_icon_uv = |index: u32| {
            let x0 = resource_x + index * icon_cell_w;
            let x1 = (resource_x + (index + 1) * icon_cell_w).min(resource_x + resource_atlas_w);
            [(x0 as f32) / atlas_wf, 0.0, (x1 as f32) / atlas_wf, icon_v1]
        };
        uv_by_kind.insert(IconKind::Emerald, resource_icon_uv(0));
        uv_by_kind.insert(IconKind::Ore, resource_icon_uv(1));
        uv_by_kind.insert(IconKind::Crops, resource_icon_uv(2));
        uv_by_kind.insert(IconKind::Fish, resource_icon_uv(3));
        uv_by_kind.insert(IconKind::Wood, resource_icon_uv(4));
        uv_by_kind.insert(IconKind::Rainbow, resource_icon_uv(5));
        uv_by_kind.insert(
            IconKind::HqCrown,
            [
                (crown_x as f32) / atlas_wf,
                0.0,
                ((crown_x + crown_slot_w) as f32) / atlas_wf,
                icon_v1,
            ],
        );
        let (ornament_uv, ornament_aspect) = if orn_found {
            let tight_w = (orn_max_x - orn_min_x + 1).max(1);
            let tight_h = (orn_max_y - orn_min_y + 1).max(1);
            (
                [
                    ((ornament_x + orn_min_x) as f32) / atlas_wf,
                    (orn_min_y as f32) / atlas_hf,
                    ((ornament_x + orn_max_x + 1) as f32) / atlas_wf,
                    ((orn_max_y + 1) as f32) / atlas_hf,
                ],
                (tight_w as f32 / tight_h as f32).clamp(0.2, 5.0),
            )
        } else {
            (
                [
                    (ornament_x as f32) / atlas_wf,
                    0.0,
                    ((ornament_x + ornament_w) as f32) / atlas_wf,
                    (ornament_h as f32) / atlas_hf,
                ],
                (ornament_w as f32 / ornament_h as f32).clamp(0.2, 5.0),
            )
        };
        uv_by_kind.insert(IconKind::TerritoryOrnament, ornament_uv);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("icon-atlas-tex"),
            size: wgpu::Extent3d {
                width: atlas_w,
                height: atlas_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * atlas_w),
                rows_per_image: Some(atlas_h),
            },
            wgpu::Extent3d {
                width: atlas_w,
                height: atlas_h,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("icon-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("icon-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("icon-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let icon_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("icon-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("icon.wgsl").into()),
        });
        let icon_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<IconInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };
        let icon_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("icon-pl"),
            bind_group_layouts: &[viewport_bind_group_layout, &bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("icon-pipeline"),
            layout: Some(&icon_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &icon_shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone(), icon_instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &icon_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let initial_capacity = 2048u32;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("icon-instance-buf"),
            size: (initial_capacity as u64) * std::mem::size_of::<IconInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(GpuIconRenderer {
            pipeline,
            bind_group,
            instance_buffer,
            instance_count: 0,
            instance_capacity: initial_capacity,
            instances_buf: Vec::new(),
            uv_by_kind,
            ornament_aspect,
        })
    }

    fn build_glyph_atlas(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        readable_font: bool,
    ) -> Option<(
        wgpu::BindGroupLayout,
        wgpu::BindGroup,
        wgpu::BindGroup,
        HashMap<char, GlyphMeta>,
        HashMap<u32, f32>,
        f32,
    )> {
        let document = web_sys::window()?.document()?;
        let canvas = document
            .create_element("canvas")
            .ok()?
            .dyn_into::<HtmlCanvasElement>()
            .ok()?;
        let ctx = get_2d_context(&canvas, true)?;

        let chars: Vec<char> = GLYPH_ATLAS_CHARS.chars().collect();
        if chars.is_empty() {
            return None;
        }
        let font = if readable_font {
            format!("{}px 'Inter', system-ui, sans-serif", GLYPH_ATLAS_FONT_PX)
        } else {
            format!("{}px 'SilkscreenLocal', monospace", GLYPH_ATLAS_FONT_PX)
        };
        ctx.set_font(&font);
        ctx.set_text_align("left");
        ctx.set_text_baseline("alphabetic");

        let mut max_advance = 0.0f64;
        let mut max_left = 0.0f64;
        let mut max_right = 0.0f64;
        let mut max_ascent = 0.0f64;
        let mut max_descent = 0.0f64;
        let mut advances: HashMap<char, f32> = HashMap::with_capacity(chars.len());
        let mut ink_bounds_x: HashMap<char, (f32, f32)> = HashMap::with_capacity(chars.len());
        let mut ink_bounds_y: HashMap<char, (f32, f32)> = HashMap::with_capacity(chars.len());
        for &ch in &chars {
            let text = ch.to_string();
            let metrics = ctx.measure_text(&text).ok();
            let adv = metrics
                .as_ref()
                .map(|m| m.width())
                .unwrap_or(GLYPH_ATLAS_FONT_PX * 0.55)
                .max(1.0);
            let measured_left = metrics
                .as_ref()
                .map(|m| m.actual_bounding_box_left())
                .unwrap_or(0.0)
                .max(0.0);
            let measured_right = metrics
                .as_ref()
                .map(|m| m.actual_bounding_box_right())
                .unwrap_or(adv)
                .max(1.0);
            let measured_ascent = metrics
                .as_ref()
                .map(|m| m.actual_bounding_box_ascent())
                .unwrap_or(GLYPH_ATLAS_FONT_PX * 0.78)
                .max(1.0);
            let measured_descent = metrics
                .as_ref()
                .map(|m| m.actual_bounding_box_descent())
                .unwrap_or(GLYPH_ATLAS_FONT_PX * 0.22)
                .max(0.0);
            let left = measured_left as f32;
            let right = measured_right.max(adv - measured_left) as f32;
            let ascent = measured_ascent as f32;
            let descent = measured_descent as f32;
            max_advance = max_advance.max(adv);
            max_left = max_left.max(measured_left);
            max_right = max_right.max(measured_right);
            max_ascent = max_ascent.max(measured_ascent);
            max_descent = max_descent.max(measured_descent);
            advances.insert(ch, adv as f32);
            ink_bounds_x.insert(ch, (left, right));
            ink_bounds_y.insert(ch, (ascent, descent));
        }

        let mut kerning = HashMap::new();
        for &a in &chars {
            for &b in &chars {
                let pair = format!("{a}{b}");
                let pair_w = ctx.measure_text(&pair).map(|m| m.width()).unwrap_or(0.0) as f32;
                let aw = advances.get(&a).copied().unwrap_or(0.0);
                let bw = advances.get(&b).copied().unwrap_or(0.0);
                let kern = pair_w - (aw + bw);
                if kern.abs() > 0.01 {
                    kerning.insert(kerning_key(a, b), kern);
                }
            }
        }

        let line_height_px = (max_ascent + max_descent).max(GLYPH_ATLAS_FONT_PX);
        let stroke_px = ((GLYPH_ATLAS_FONT_PX * GLYPH_ATLAS_STROKE_FACTOR)
            .max(GLYPH_ATLAS_STROKE_MIN_PX)) as f32;
        let ink_bleed_px = stroke_px * GLYPH_ATLAS_BLEED_FACTOR + GLYPH_ATLAS_BLEED_EXTRA_PX;
        let cell_w = (max_advance + max_left + max_right + GLYPH_ATLAS_PADDING_PX * 2.0)
            .ceil()
            .max(GLYPH_ATLAS_FONT_PX * 0.7);
        let cell_h = (line_height_px + GLYPH_ATLAS_PADDING_PX * 2.0).ceil();
        let cols = GLYPH_ATLAS_COLS;
        let rows = chars.len().div_ceil(cols);
        let atlas_w = (cell_w as usize * cols).max(1) as u32;
        let atlas_h = (cell_h as usize * rows).max(1) as u32;
        canvas.set_width(atlas_w);
        canvas.set_height(atlas_h);
        ctx.set_font(&font);
        ctx.set_text_align("left");
        ctx.set_text_baseline("alphabetic");

        let mut glyphs = HashMap::with_capacity(chars.len());
        let atlas_wf = atlas_w as f32;
        let atlas_hf = atlas_h as f32;
        for (i, ch) in chars.iter().copied().enumerate() {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            let x = col * cell_w;
            let y = row * cell_h;
            let raw_advance = advances
                .get(&ch)
                .copied()
                .unwrap_or((cell_w - GLYPH_ATLAS_PADDING_PX * 2.0).max(1.0) as f32);
            let (left, right) = ink_bounds_x
                .get(&ch)
                .copied()
                .unwrap_or((0.0, raw_advance.max(1.0)));
            let (ascent, descent) = ink_bounds_y
                .get(&ch)
                .copied()
                .unwrap_or((line_height_px as f32 * 0.78, line_height_px as f32 * 0.22));
            let draw_offset_x = -left - ink_bleed_px;
            let draw_width = (left + right + ink_bleed_px * 2.0)
                .max(raw_advance)
                .max(1.0);
            let draw_offset_y = (max_ascent as f32 - ascent) - ink_bleed_px;
            let draw_height = (ascent + descent + ink_bleed_px * 2.0).max(1.0);
            let inset_x = (GLYPH_ATLAS_PADDING_PX * 0.25).max(0.5);
            let inset_y = (GLYPH_ATLAS_PADDING_PX * 0.15).max(0.5);
            let u0_px = (x + GLYPH_ATLAS_PADDING_PX + draw_offset_x as f64)
                .max(x + inset_x)
                .min(x + cell_w - 0.5);
            let u1_px = (x + GLYPH_ATLAS_PADDING_PX + (draw_offset_x + draw_width) as f64)
                .min(x + cell_w - 0.25)
                .max(u0_px + 0.5);
            let v0_px = (y + GLYPH_ATLAS_PADDING_PX + draw_offset_y as f64)
                .max(y + inset_y)
                .min(y + cell_h - 0.5);
            let v1_px = (y + GLYPH_ATLAS_PADDING_PX + (draw_offset_y + draw_height) as f64)
                .min(y + cell_h - 0.25)
                .max(v0_px + 0.5);
            let u0 = (u0_px as f32) / atlas_wf;
            let v0 = (v0_px as f32) / atlas_hf;
            let u1 = (u1_px as f32) / atlas_wf;
            let v1 = (v1_px as f32) / atlas_hf;
            glyphs.insert(
                ch,
                GlyphMeta {
                    uv_rect: [u0, v0, u1, v1],
                    advance: raw_advance,
                    draw_offset_x,
                    draw_width,
                    draw_offset_y,
                    draw_height,
                },
            );
        }

        ctx.clear_rect(0.0, 0.0, atlas_w as f64, atlas_h as f64);
        ctx.set_fill_style_str("rgba(255,255,255,1.0)");
        for (i, ch) in chars.iter().copied().enumerate() {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            let x = col * cell_w;
            let y = row * cell_h;
            let baseline_y = y + GLYPH_ATLAS_PADDING_PX + max_ascent;
            ctx.fill_text(&ch.to_string(), x + GLYPH_ATLAS_PADDING_PX, baseline_y)
                .ok()?;
        }
        let fill_pixels = ctx
            .get_image_data(0.0, 0.0, atlas_w as f64, atlas_h as f64)
            .ok()?
            .data();

        ctx.clear_rect(0.0, 0.0, atlas_w as f64, atlas_h as f64);
        ctx.set_stroke_style_str("rgba(255,255,255,1.0)");
        ctx.set_line_join("round");
        ctx.set_line_cap("round");
        ctx.set_line_width(stroke_px as f64);
        for (i, ch) in chars.iter().copied().enumerate() {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            let x = col * cell_w;
            let y = row * cell_h;
            let baseline_y = y + GLYPH_ATLAS_PADDING_PX + max_ascent;
            ctx.stroke_text(&ch.to_string(), x + GLYPH_ATLAS_PADDING_PX, baseline_y)
                .ok()?;
        }
        let halo_pixels = ctx
            .get_image_data(0.0, 0.0, atlas_w as f64, atlas_h as f64)
            .ok()?
            .data();

        let mip_levels = glyph_mip_level_count(atlas_w, atlas_h);
        let make_texture = |label: &'static str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: atlas_w,
                    height: atlas_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: mip_levels,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let fill_texture = make_texture("glyph-atlas-fill-tex");
        let halo_texture = make_texture("glyph-atlas-halo-tex");

        write_texture_mip_chain(queue, &fill_texture, &fill_pixels, atlas_w, atlas_h);
        write_texture_mip_chain(queue, &halo_texture, &halo_pixels, atlas_w, atlas_h);

        let fill_view = fill_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let halo_view = halo_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph-atlas-sampler"),
            // Linear filtering reduces shimmer/aliasing when zooming text at non-integer scales.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            lod_max_clamp: (mip_levels.saturating_sub(1)) as f32,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let fill_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text-fill-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&fill_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let halo_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text-halo-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&halo_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Some((
            bind_group_layout,
            fill_bind_group,
            halo_bind_group,
            glyphs,
            kerning,
            line_height_px as f32,
        ))
    }

    /// Mark instance data as needing a rebuild (territory/hover/select/settings changed).
    #[allow(dead_code)]
    pub fn mark_instance_dirty(&mut self) {
        self.mark_dirty(InvalidationReason::Geometry);
    }

    /// Mark static label instances as needing a rebuild.
    #[allow(dead_code)]
    pub fn mark_text_dirty(&mut self) {
        self.mark_dirty(InvalidationReason::StaticLabel);
    }

    #[allow(dead_code)]
    pub fn mark_dynamic_text_dirty(&mut self) {
        self.mark_dirty(InvalidationReason::DynamicLabel);
    }

    #[allow(dead_code)]
    pub fn mark_icon_dirty(&mut self) {
        self.mark_dirty(InvalidationReason::Resources);
    }

    #[allow(dead_code)]
    pub fn mark_connection_dirty(&mut self) {
        self.mark_dirty(InvalidationReason::Resources);
    }

    pub fn mark_dirty(&mut self, reason: InvalidationReason) {
        match reason {
            InvalidationReason::Geometry => self.instance_dirty = true,
            InvalidationReason::StaticLabel => self.static_text_dirty = true,
            InvalidationReason::DynamicLabel => self.dynamic_text_dirty = true,
            InvalidationReason::Viewport => {
                self.dynamic_text_dirty = true;
                self.icon_dirty = true;
            }
            InvalidationReason::Resources => {
                self.icon_dirty = true;
                self.connection_dirty = true;
            }
        }
    }

    pub fn capabilities(&self) -> RenderCapabilities {
        self.capabilities
    }

    pub fn frame_metrics(&self) -> FrameMetrics {
        self.frame_metrics
    }

    #[allow(dead_code)]
    pub fn supports_static_gpu_labels(&self) -> bool {
        self.text_renderer.is_some()
    }

    /// Resize the surface when the canvas size changes.
    pub fn resize(&mut self, width: u32, height: u32, dpr: f32) {
        if width == 0 || height == 0 {
            return;
        }
        let size_changed =
            self.surface_config.width != width || self.surface_config.height != height;
        self.width = width;
        self.height = height;
        self.dpr = dpr;
        if !size_changed {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    fn ensure_tile_upload_context(&mut self) -> bool {
        if self.tile_upload_canvas.is_some() && self.tile_upload_ctx.is_some() {
            return true;
        }
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            web_sys::console::warn_1(
                &"Skipping tile upload: document is unavailable for upload canvas".into(),
            );
            return false;
        };
        let Some(canvas) = document
            .create_element("canvas")
            .ok()
            .and_then(|element| element.dyn_into::<HtmlCanvasElement>().ok())
        else {
            web_sys::console::warn_1(
                &"Skipping tile upload: failed to create upload canvas".into(),
            );
            return false;
        };
        let Some(ctx) = get_2d_context(&canvas, true) else {
            web_sys::console::warn_1(
                &"Skipping tile upload: failed to create upload 2d context".into(),
            );
            return false;
        };
        self.tile_upload_canvas = Some(canvas);
        self.tile_upload_ctx = Some(ctx);
        self.tile_upload_canvas_size = (0, 0);
        true
    }

    /// Upload tile images as GPU textures with pre-baked rect uniforms.
    pub fn upload_tiles(&mut self, tiles: &[LoadedTile]) {
        if !self.ensure_tile_upload_context() {
            return;
        }
        let active_tile_ids: HashSet<usize> = tiles.iter().map(|tile| tile.id).collect();
        self.tile_textures
            .retain(|tile_id, _| active_tile_ids.contains(tile_id));

        let Some(upload_canvas) = self.tile_upload_canvas.as_ref().cloned() else {
            return;
        };
        let Some(upload_ctx) = self.tile_upload_ctx.as_ref().cloned() else {
            return;
        };
        let mut upload_size = self.tile_upload_canvas_size;

        for tile in tiles {
            let tile_id = tile.id;
            if let Some(existing) = self.tile_textures.get(&tile_id)
                && existing.quality >= tile.quality
            {
                continue;
            }

            let img = &tile.image;
            let w = img.natural_width();
            let h = img.natural_height();
            if w == 0 || h == 0 {
                continue;
            }

            // Reuse a persistent staging canvas/context to avoid per-tile DOM/context churn.
            if upload_size != (w, h) {
                upload_canvas.set_width(w);
                upload_canvas.set_height(h);
                upload_size = (w, h);
            }
            upload_ctx.clear_rect(0.0, 0.0, w as f64, h as f64);
            upload_ctx
                .draw_image_with_html_image_element(img, 0.0, 0.0)
                .ok();
            let image_data = match upload_ctx.get_image_data(0.0, 0.0, w as f64, h as f64) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let pixels = image_data.data();

            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tile-tex"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });

            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );

            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Pre-compute tile world rect and bake into a dedicated uniform buffer
            let x1 = tile.x1.min(tile.x2) as f32;
            let z1 = tile.z1.min(tile.z2) as f32;
            // Tile bounds are inclusive — add 1 to get exclusive width/height
            let tw = (tile.x1.max(tile.x2) - tile.x1.min(tile.x2) + 1) as f32;
            let th = (tile.z1.max(tile.z2) - tile.z1.min(tile.z2) + 1) as f32;
            let rect = [x1, z1, tw, th];

            let rect_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("tile-rect-ubo"),
                    contents: bytemuck::cast_slice(&[TileRectUniform { rect }]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tile-bg"),
                layout: &self.tile_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: rect_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.tile_sampler),
                    },
                ],
            });

            self.tile_textures.insert(
                tile_id,
                TileTexture {
                    bind_group,
                    rect,
                    quality: tile.quality,
                },
            );
        }
        self.tile_upload_canvas_size = upload_size;
        self.tile_world_bounds = Self::tile_world_bounds(tiles);
    }

    /// Build instance data from territories and upload to GPU.
    ///
    /// Animation color interpolation is handled GPU-side: we encode
    /// from_color + timing in the instance data once, and the shader
    /// computes the interpolated color every frame at zero CPU cost.
    fn update_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        hovered: &Option<String>,
        selected: &Option<String>,
        now: f64,
        thick_cooldown_borders: bool,
        heat_mode_enabled: bool,
        heat_entries: &HashMap<String, u64>,
        heat_max_take_count: u64,
    ) {
        let start_ms = self.start_time_ms;
        let start_secs = start_ms / 1000.0;

        self.instances_buf.clear();
        self.instances_buf
            .extend(territories.iter().map(|(name, ct)| {
                let loc = &ct.territory.location;
                let (r, g, b) = if heat_mode_enabled {
                    let take_count = heat_entries.get(name).copied().unwrap_or(0);
                    heat_color_for_count(take_count, heat_max_take_count)
                } else {
                    ct.guild_color
                };

                let is_hovered = hovered.as_deref() == Some(name.as_str());
                let is_selected = selected.as_deref() == Some(name.as_str());

                let resource_data = if self.resource_highlight {
                    ct.territory.resources.highlight_data()
                } else {
                    [0.0; 4]
                };
                let has_resource =
                    resource_data[0] > 0.5 || (resource_data[3] as u32 & (1 << 10)) != 0; // mode 0 + double emeralds

                let fill_alpha = if has_resource {
                    if is_selected {
                        0.52
                    } else if is_hovered {
                        0.44
                    } else {
                        0.34
                    }
                } else if is_selected {
                    0.38
                } else if is_hovered {
                    0.33
                } else {
                    0.26
                };

                let is_headquarters = ct
                    .territory
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.headquarters)
                    .unwrap_or(false);
                let flags =
                    (is_hovered as u32) + (is_selected as u32) * 2 + (is_headquarters as u32) * 4;

                let acquired_rel_secs =
                    (ct.territory.acquired.timestamp() as f64 - start_secs) as f32;

                // Encode animation params for GPU-side interpolation
                let (anim_color, anim_time) = if heat_mode_enabled {
                    ([0.0; 4], [0.0; 4])
                } else {
                    match ct.animation.as_ref() {
                        Some(anim) if anim.current_color(now).is_some() => {
                            let (fr, fg, fb) =
                                hsl_to_rgb(anim.from_hsl.0, anim.from_hsl.1, anim.from_hsl.2);
                            let rel_start = ((anim.start_time - start_ms) / 1000.0) as f32;
                            let dur_secs = (anim.duration / 1000.0) as f32;
                            (
                                [fr as f32 / 255.0, fg as f32 / 255.0, fb as f32 / 255.0, 0.0],
                                [rel_start, dur_secs, 0.0, 0.0],
                            )
                        }
                        _ => ([0.0; 4], [0.0; 4]),
                    }
                };

                TerritoryInstance {
                    rect: [
                        loc.left() as f32,
                        loc.top() as f32,
                        loc.width() as f32,
                        loc.height() as f32,
                    ],
                    color: [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0],
                    state: [
                        fill_alpha,
                        0.72,
                        flags as f32,
                        if thick_cooldown_borders { 2.0 } else { 1.0 },
                    ],
                    cooldown: [acquired_rel_secs, 0.0, 0.0, 0.0],
                    anim_color,
                    anim_time,
                    resource_data,
                }
            }));

        self.instance_count = self.instances_buf.len() as u32;

        // Cache the latest animation end time so render() can check
        // has_anims with a single comparison instead of scanning all territories.
        self.max_anim_end_ms = territories
            .values()
            .filter_map(|ct| ct.animation.as_ref())
            .map(|a| a.start_time + a.duration)
            .fold(0.0f64, f64::max);

        if self.instance_count > self.instance_capacity {
            self.instance_capacity = self.instance_count.next_power_of_two();
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instance-buf"),
                size: (self.instance_capacity as u64)
                    * std::mem::size_of::<TerritoryInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.instances_buf.is_empty() {
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances_buf),
            );
        }
    }

    fn upload_text_buffer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        instances: &[TextInstance],
        buffer: &mut wgpu::Buffer,
        count: &mut u32,
        capacity: &mut u32,
    ) {
        *count = instances.len() as u32;
        if *count > *capacity {
            *capacity = (*count).next_power_of_two();
            *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (*capacity as u64) * std::mem::size_of::<TextInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !instances.is_empty() {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    fn upload_icon_buffer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[IconInstance],
        instance_buffer: &mut wgpu::Buffer,
        instance_count: &mut u32,
        instance_capacity: &mut u32,
    ) {
        *instance_count = instances.len() as u32;
        if *instance_count > *instance_capacity {
            *instance_capacity = (*instance_count).next_power_of_two();
            *instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("icon-instance-buf"),
                size: (*instance_capacity as u64) * std::mem::size_of::<IconInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !instances.is_empty() {
            queue.write_buffer(instance_buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    fn dynamic_zoom_bucket(scale: f64) -> i32 {
        (scale * 20.0).floor() as i32
    }

    fn static_zoom_bucket(scale: f64) -> i32 {
        // Finer bucketing minimizes visible stepping while zooming labels.
        (scale * 320.0).floor() as i32
    }

    #[inline]
    fn effective_scale(master: f32, group: f32) -> f32 {
        let master = if master.is_finite() { master } else { 1.0 };
        let group = if group.is_finite() { group } else { 1.0 };
        (master * group).clamp(0.5, 4.0)
    }

    #[inline]
    fn effective_static_tag_scale(&self) -> f32 {
        Self::effective_scale(self.label_scale_master, self.label_scale_static_tag)
    }

    #[inline]
    fn effective_static_name_scale(&self) -> f32 {
        Self::effective_scale(self.label_scale_master, self.label_scale_static_name)
    }

    #[inline]
    fn effective_dynamic_label_scale(&self) -> f32 {
        Self::effective_scale(self.label_scale_master, self.label_scale_dynamic)
    }

    #[inline]
    fn effective_icon_scale(&self) -> f32 {
        Self::effective_scale(self.label_scale_master, self.label_scale_icons)
    }

    fn sync_territory_name_cache(&mut self, territories: &ClientTerritoryMap) {
        self.territory_name_cache
            .retain(|name, _| territories.contains_key(name));
        for name in territories.keys() {
            self.territory_name_cache
                .entry(name.clone())
                .or_insert_with(|| (abbreviate_name(name), name.clone()));
        }
    }

    /// Build static text glyph instances (guild tag + optional territory name).
    fn update_static_text_instances(&mut self, territories: &ClientTerritoryMap, vp: &Viewport) {
        self.sync_territory_name_cache(territories);
        let static_tag_scale = self.effective_static_tag_scale();
        let static_name_scale = self.effective_static_name_scale();
        let Some(text_renderer) = self.text_renderer.as_mut() else {
            self.static_text_dirty = false;
            return;
        };

        let mut fill_instances = std::mem::take(&mut text_renderer.static_fill_instances);
        let mut halo_instances = std::mem::take(&mut text_renderer.static_halo_instances);
        fill_instances.clear();
        halo_instances.clear();

        if !self.use_static_gpu_labels || vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            text_renderer.static_fill_instances = fill_instances;
            text_renderer.static_halo_instances = halo_instances;
            text_renderer.static_fill_count = 0;
            text_renderer.static_halo_count = 0;
            self.static_text_dirty = false;
            return;
        }

        let scale = vp.scale as f32;
        {
            let glyphs = &text_renderer.glyphs;
            let kerning = &text_renderer.kerning;
            let line_height = text_renderer.line_height;
            let tag_tracking_units = line_height * STATIC_TAG_LETTER_SPACING_EM;
            let name_tracking_units = line_height * STATIC_NAME_LETTER_SPACING_EM;
            for (name, ct) in territories {
                let loc = &ct.territory.location;
                let ww = loc.width() as f32;
                let hh = loc.height() as f32;
                let Some(sizing) = compute_static_label_sizing(ww, hh, scale) else {
                    continue;
                };
                let cx = loc.midpoint_x() as f32;
                let cy = loc.midpoint_y() as f32;
                let detail_layout_alpha = sizing.detail_layout_alpha;
                let tag_size = sizing.tag_size * static_tag_scale;
                let detail_size = sizing.detail_size * static_name_scale;
                let px_per_world = scale.max(0.0001);
                // Keep static label sizing stable in world space; only user scale settings widen
                // the fit budget a bit to accommodate larger configured text.
                let max_static_scale = static_tag_scale.max(static_name_scale);
                let overflow_scale =
                    (1.0 + (max_static_scale - 1.0).max(0.0) * 0.22).clamp(1.0, 1.35);
                let overflow = overflow_scale;
                let tag_padding = lerp_f32(3.0, 8.0, detail_layout_alpha);
                let tag_max_w = (ww * overflow - tag_padding).max(STATIC_TAG_MIN_WIDTH_WORLD);
                let tag_y = lerp_f32(cy, cy - (detail_size + 1.0) * 0.45, detail_layout_alpha);
                let tag = ct.territory.guild.prefix.as_str();
                let tag_color = {
                    let mut c = name_color_rgba(self.static_tag_color, ct.guild_color);
                    c[3] = 1.0;
                    c
                };
                let tag_px = tag_size * px_per_world;
                let tag_halo_boost = 1.0 - smoothstep_f32(9.6, 13.8, tag_px);
                let tag_halo_alpha = (0.70 - tag_halo_boost * 0.10).clamp(0.54, 0.76);

                push_text_line_dual_with_tracking(
                    &mut fill_instances,
                    &mut halo_instances,
                    glyphs,
                    kerning,
                    line_height,
                    tag,
                    cx,
                    tag_y,
                    tag_size,
                    tag_max_w,
                    tag_tracking_units,
                    tag_color,
                    [0.0, 0.0, 0.0, tag_halo_alpha],
                );

                if self.static_show_names && detail_layout_alpha > 0.02 {
                    let fallback_abbrev;
                    let base_name = if let Some((abbreviated, full)) =
                        self.territory_name_cache.get(name.as_str())
                    {
                        if self.static_abbreviate_names {
                            abbreviated.as_str()
                        } else {
                            full.as_str()
                        }
                    } else if self.static_abbreviate_names {
                        fallback_abbrev = abbreviate_name(name);
                        fallback_abbrev.as_str()
                    } else {
                        name.as_str()
                    };
                    let name_max_w = (ww * overflow - 10.0).max(STATIC_NAME_MIN_WIDTH_WORLD);
                    let units_per_world = line_height / detail_size.max(0.001);
                    let fitted = fit_text_to_units_with_tracking(
                        base_name,
                        name_max_w * units_per_world,
                        glyphs,
                        kerning,
                        name_tracking_units,
                    );
                    let name_y =
                        tag_y + tag_size * 0.5 + detail_size * STATIC_NAME_BASELINE_GAP_MULTIPLIER;
                    let mut name_rgba = name_color_rgba(self.static_name_color, ct.guild_color);
                    name_rgba[3] *= detail_layout_alpha.clamp(0.0, 1.0);
                    let name_px = detail_size * px_per_world;
                    let name_halo_boost = 1.0 - smoothstep_f32(8.4, 12.2, name_px);
                    let name_halo_alpha = ((0.68 - name_halo_boost * 0.12)
                        * detail_layout_alpha.clamp(0.0, 1.0))
                    .clamp(0.0, 0.74);
                    push_text_line_dual_with_tracking(
                        &mut fill_instances,
                        &mut halo_instances,
                        glyphs,
                        kerning,
                        line_height,
                        &fitted,
                        cx,
                        name_y,
                        detail_size,
                        name_max_w,
                        name_tracking_units,
                        name_rgba,
                        [0.0, 0.0, 0.0, name_halo_alpha],
                    );
                }
            }
        }

        text_renderer.static_fill_instances = fill_instances;
        text_renderer.static_halo_instances = halo_instances;

        Self::upload_text_buffer(
            &self.device,
            &self.queue,
            "text-static-fill-buf",
            &text_renderer.static_fill_instances,
            &mut text_renderer.static_fill_buffer,
            &mut text_renderer.static_fill_count,
            &mut text_renderer.static_fill_capacity,
        );
        Self::upload_text_buffer(
            &self.device,
            &self.queue,
            "text-static-halo-buf",
            &text_renderer.static_halo_instances,
            &mut text_renderer.static_halo_buffer,
            &mut text_renderer.static_halo_count,
            &mut text_renderer.static_halo_capacity,
        );

        self.diag_static_rebuilds = self.diag_static_rebuilds.saturating_add(1);
        self.static_text_dirty = false;
    }

    fn update_dynamic_text_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        vp: &Viewport,
        reference_time_secs: i64,
    ) {
        let static_tag_scale = self.effective_static_tag_scale();
        let static_name_scale = self.effective_static_name_scale();
        let dynamic_label_scale = self.effective_dynamic_label_scale();
        let Some(text_renderer) = self.text_renderer.as_mut() else {
            self.dynamic_text_dirty = false;
            return;
        };

        let mut fill_instances = std::mem::take(&mut text_renderer.dynamic_fill_instances);
        let mut halo_instances = std::mem::take(&mut text_renderer.dynamic_halo_instances);
        fill_instances.clear();
        halo_instances.clear();

        if !self.use_full_gpu_text || vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            text_renderer.dynamic_fill_instances = fill_instances;
            text_renderer.dynamic_halo_instances = halo_instances;
            text_renderer.dynamic_fill_count = 0;
            text_renderer.dynamic_halo_count = 0;
            self.dynamic_text_dirty = false;
            return;
        }

        let scale = vp.scale as f32;
        let mut next_update_secs = i64::MAX;
        let mut text_buf = String::with_capacity(16);
        {
            let glyphs = &text_renderer.glyphs;
            let kerning = &text_renderer.kerning;
            let line_height = text_renderer.line_height;
            for (_name, ct) in territories {
                let loc = &ct.territory.location;
                let ww = loc.width() as f32;
                let hh = loc.height() as f32;
                let sw = ww * scale;
                let sh = hh * scale;
                let state =
                    dynamic_text_state(reference_time_secs, ct.territory.acquired.timestamp());
                let next_age = dynamic_label_next_update_age(
                    state.age_secs,
                    self.dynamic_show_countdown,
                    self.dynamic_show_granular_map_time,
                    self.dynamic_show_compound_map_time,
                );
                next_update_secs =
                    next_update_secs.min(ct.territory.acquired.timestamp() + next_age);

                let metrics = compute_label_layout_metrics(sw as f64, sh as f64, false);
                let detail_layout_alpha = metrics.detail_layout_alpha;

                let Some(sizing) = compute_dynamic_label_sizing(
                    ww,
                    hh,
                    scale,
                    dynamic_label_scale,
                    state.is_fresh,
                ) else {
                    continue;
                };
                let px_per_world = scale.max(0.0001);
                let small_timer_factor = sizing.small_timer_factor;
                let tag_size = sizing.tag_size;
                let detail_size = sizing.detail_size;
                let time_size = sizing.time_size;
                let cooldown_size = sizing.cooldown_size;
                let line_gap = sizing.line_gap;
                let time_tracking_units = line_height * DYNAMIC_TIME_LETTER_SPACING_EM;
                let cooldown_tracking_units = line_height
                    * lerp_f32(
                        DYNAMIC_TIME_LETTER_SPACING_EM,
                        DYNAMIC_COOLDOWN_LETTER_SPACING_EM_MIN,
                        small_timer_factor,
                    );
                let time_max_width = sizing.time_max_width;
                let cooldown_max_width = sizing.cooldown_max_width;

                let cx = loc.midpoint_x() as f32;
                let cy = loc.midpoint_y() as f32;
                let timer_visible_at_zoom = vp.scale >= DYNAMIC_TIMER_VISIBILITY_MIN_SCALE;
                let show_dynamic_cooldown =
                    timer_visible_at_zoom && self.dynamic_show_countdown && state.is_fresh;
                let show_dynamic_time = timer_visible_at_zoom && !show_dynamic_cooldown;
                let has_timer_line = show_dynamic_time || show_dynamic_cooldown;
                let static_name_bottom = static_name_bottom_bound(
                    self.use_static_gpu_labels,
                    self.static_show_names,
                    ww,
                    hh,
                    cy,
                    scale,
                    static_tag_scale,
                    static_name_scale,
                );
                let compact_bottom_y = cy + tag_size / 2.0;
                let mut time_y = compact_bottom_y;
                let mut content_bottom_y = compact_bottom_y;
                if has_timer_line {
                    let stacked_total_h = tag_size + detail_size + time_size + line_gap * 2.0;
                    let stacked_top_y = cy - stacked_total_h / 2.0;
                    time_y = stacked_top_y
                        + tag_size
                        + line_gap
                        + detail_size
                        + line_gap
                        + time_size / 2.0;
                    if let Some(name_bottom_y) = static_name_bottom {
                        let min_time_y = name_bottom_y + time_size * 0.5 + line_gap * 0.8;
                        time_y = time_y.max(min_time_y);
                    }
                    let stacked_bottom_y = time_y + time_size / 2.0;
                    content_bottom_y =
                        lerp_f32(compact_bottom_y, stacked_bottom_y, detail_layout_alpha);
                }
                if let Some(name_bottom_y) = static_name_bottom {
                    content_bottom_y = content_bottom_y.max(name_bottom_y + line_gap * 0.6);
                }

                if show_dynamic_time {
                    if self.dynamic_show_granular_map_time {
                        write_hms(&mut text_buf, state.age_secs);
                    } else if self.dynamic_show_compound_map_time {
                        write_age_compound(&mut text_buf, state.age_secs);
                    } else {
                        write_age(&mut text_buf, state.age_secs);
                    }
                    let fill_color = if state.is_fresh {
                        let urgency = 1.0 - state.cooldown_frac as f64;
                        let (cr, cg, cb) = cooldown_color(urgency);
                        [
                            cr as f32 / 255.0,
                            cg as f32 / 255.0,
                            cb as f32 / 255.0,
                            0.95,
                        ]
                    } else {
                        let (tr, tg, tb) =
                            TreasuryLevel::from_held_seconds(state.age_secs).color_rgb();
                        [
                            tr as f32 / 255.0,
                            tg as f32 / 255.0,
                            tb as f32 / 255.0,
                            0.95,
                        ]
                    };
                    push_text_line_dual_with_tracking(
                        &mut fill_instances,
                        &mut halo_instances,
                        glyphs,
                        kerning,
                        line_height,
                        &text_buf,
                        cx,
                        time_y,
                        time_size,
                        time_max_width,
                        time_tracking_units,
                        fill_color,
                        [
                            0.0,
                            0.0,
                            0.0,
                            (0.68
                                - (1.0 - smoothstep_f32(9.5, 14.8, time_size * px_per_world))
                                    * 0.12)
                                .clamp(0.54, 0.76),
                        ],
                    );
                }

                if show_dynamic_cooldown {
                    let remaining = 600 - state.age_secs;
                    text_buf.clear();
                    let _ = write!(&mut text_buf, "{}:{:02}", remaining / 60, remaining % 60);
                    let cooldown_gap = 3.5 + line_gap * 0.35;
                    let cd_y = content_bottom_y + cooldown_size / 2.0 + cooldown_gap;
                    let urgency = 1.0 - state.cooldown_frac as f64;
                    let (cr, cg, cb) = cooldown_color(urgency);
                    let cd_alpha =
                        (0.95 + urgency as f32 * 0.05 + small_timer_factor * 0.02).clamp(0.0, 1.0);
                    push_text_line_dual_with_tracking(
                        &mut fill_instances,
                        &mut halo_instances,
                        glyphs,
                        kerning,
                        line_height,
                        &text_buf,
                        cx,
                        cd_y,
                        cooldown_size,
                        cooldown_max_width,
                        cooldown_tracking_units,
                        [
                            cr as f32 / 255.0,
                            cg as f32 / 255.0,
                            cb as f32 / 255.0,
                            cd_alpha,
                        ],
                        [
                            0.0,
                            0.0,
                            0.0,
                            (0.70
                                - (1.0 - smoothstep_f32(10.2, 15.8, cooldown_size * px_per_world))
                                    * 0.12)
                                .clamp(0.56, 0.78),
                        ],
                    );
                }
            }
        }

        text_renderer.dynamic_fill_instances = fill_instances;
        text_renderer.dynamic_halo_instances = halo_instances;

        Self::upload_text_buffer(
            &self.device,
            &self.queue,
            "text-dynamic-fill-buf",
            &text_renderer.dynamic_fill_instances,
            &mut text_renderer.dynamic_fill_buffer,
            &mut text_renderer.dynamic_fill_count,
            &mut text_renderer.dynamic_fill_capacity,
        );
        Self::upload_text_buffer(
            &self.device,
            &self.queue,
            "text-dynamic-halo-buf",
            &text_renderer.dynamic_halo_instances,
            &mut text_renderer.dynamic_halo_buffer,
            &mut text_renderer.dynamic_halo_count,
            &mut text_renderer.dynamic_halo_capacity,
        );

        self.dynamic_next_update_secs = if next_update_secs == i64::MAX {
            reference_time_secs + 1
        } else {
            next_update_secs
        };
        self.diag_dynamic_rebuilds = self.diag_dynamic_rebuilds.saturating_add(1);
        self.dynamic_text_dirty = false;
    }

    fn update_icon_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        vp: &Viewport,
        reference_time_secs: i64,
    ) {
        let static_tag_scale = self.effective_static_tag_scale();
        let static_name_scale = self.effective_static_name_scale();
        let dynamic_label_scale = self.effective_dynamic_label_scale();
        let icon_scale = self.effective_icon_scale();
        let Some(renderer) = self.icon_renderer.as_mut() else {
            self.icon_dirty = false;
            return;
        };
        let ornament_uv = renderer
            .uv_by_kind
            .get(&IconKind::TerritoryOrnament)
            .copied();
        let ornament_aspect = renderer.ornament_aspect.max(0.2);
        renderer.instances_buf.clear();
        if !self.use_full_gpu_text || vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            renderer.instance_count = 0;
            self.icon_dirty = false;
            return;
        }

        let scale = vp.scale as f32;
        for (_name, ct) in territories {
            let loc = &ct.territory.location;
            let ww = loc.width() as f32;
            let hh = loc.height() as f32;
            let sw = ww * scale;
            let sh = hh * scale;
            let is_hq = ct
                .territory
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.headquarters)
                .unwrap_or(false);
            let px_per_world = scale.max(0.0001);
            let cx = loc.midpoint_x() as f32;
            let cy = loc.midpoint_y() as f32;
            if let Some(base_ornament_uv) = ornament_uv
                && sw >= ORNAMENT_MIN_BOX_PX
                && sh >= ORNAMENT_MIN_BOX_PX
            {
                let ornament_sizing =
                    compute_territory_ornament_sizing(ww, hh, ornament_aspect, icon_scale);
                let corner_w_world = ornament_sizing.corner_w_world;
                let corner_h_world = ornament_sizing.corner_h_world;
                if corner_w_world * px_per_world >= 6.0 && corner_h_world * px_per_world >= 6.0 {
                    let inset_world = ornament_sizing.inset_world;
                    let left = loc.left() as f32 + inset_world;
                    let top = loc.top() as f32 + inset_world;
                    let right = loc.left() as f32 + ww - inset_world - corner_w_world;
                    let bottom = loc.top() as f32 + hh - inset_world - corner_h_world;
                    if right < left + corner_w_world || bottom < top + corner_h_world {
                        continue;
                    }
                    let tint = compute_territory_ornament_tint(ct.guild_color);
                    renderer.instances_buf.push(IconInstance {
                        rect: [left, top, corner_w_world, corner_h_world],
                        uv_rect: base_ornament_uv,
                        tint,
                    });
                    renderer.instances_buf.push(IconInstance {
                        rect: [right, top, corner_w_world, corner_h_world],
                        uv_rect: [
                            base_ornament_uv[2],
                            base_ornament_uv[1],
                            base_ornament_uv[0],
                            base_ornament_uv[3],
                        ],
                        tint,
                    });
                    renderer.instances_buf.push(IconInstance {
                        rect: [left, bottom, corner_w_world, corner_h_world],
                        uv_rect: [
                            base_ornament_uv[0],
                            base_ornament_uv[3],
                            base_ornament_uv[2],
                            base_ornament_uv[1],
                        ],
                        tint,
                    });
                    renderer.instances_buf.push(IconInstance {
                        rect: [right, bottom, corner_w_world, corner_h_world],
                        uv_rect: [
                            base_ornament_uv[2],
                            base_ornament_uv[3],
                            base_ornament_uv[0],
                            base_ornament_uv[1],
                        ],
                        tint,
                    });
                }
            }
            if is_hq
                && let Some(crown_uv) = renderer.uv_by_kind.get(&IconKind::HqCrown).copied()
                && let Some(static_sizing) = compute_static_label_sizing(ww, hh, scale)
            {
                let crown_tag_size = static_sizing.tag_size * static_tag_scale;
                let crown_detail_size = static_sizing.detail_size * static_name_scale;
                let crown_tag_y = lerp_f32(
                    cy,
                    cy - (crown_detail_size + 1.0) * 0.45,
                    static_sizing.detail_layout_alpha,
                );
                let min_crown_world = 1.0;
                let max_crown_world = (ww.min(hh) * HQ_CROWN_MAX_BOX_FRACTION).max(min_crown_world);
                let crown_size_world = (crown_tag_size * HQ_CROWN_SIZE_MULTIPLIER)
                    .clamp(min_crown_world, max_crown_world);
                let crown_gap_world = (2.5 + crown_tag_size * 0.08).max(0.0);
                let crown_center_y =
                    crown_tag_y - crown_tag_size * 0.5 - crown_gap_world - crown_size_world * 0.5;
                renderer.instances_buf.push(IconInstance {
                    rect: [
                        cx - crown_size_world * 0.5,
                        crown_center_y - crown_size_world * 0.5,
                        crown_size_world,
                        crown_size_world,
                    ],
                    uv_rect: crown_uv,
                    tint: [1.0, 0.90, 0.30, 1.0],
                });
            }

            if !self.dynamic_show_resource_icons
                || sw <= 55.0
                || sh <= 35.0
                || ct.territory.resources.is_empty()
            {
                continue;
            }

            let icon_kinds = resource_icon_sequence(&ct.territory.resources);
            if icon_kinds.is_empty() {
                continue;
            }

            let state = dynamic_text_state(reference_time_secs, ct.territory.acquired.timestamp());
            let metrics = compute_label_layout_metrics(sw as f64, sh as f64, false);
            let detail_layout_alpha = metrics.detail_layout_alpha;
            if detail_layout_alpha <= 0.001 {
                continue;
            }

            let Some(sizing) =
                compute_dynamic_label_sizing(ww, hh, scale, dynamic_label_scale, state.is_fresh)
            else {
                continue;
            };
            let tag_size = sizing.tag_size;
            let detail_size = sizing.detail_size;
            let time_size = sizing.time_size;
            let cooldown_size = sizing.cooldown_size;
            let line_gap = sizing.line_gap;
            let timer_visible_at_zoom = vp.scale >= DYNAMIC_TIMER_VISIBILITY_MIN_SCALE;
            let cooldown_timer_visible =
                timer_visible_at_zoom && state.is_fresh && self.dynamic_show_countdown;
            let show_dynamic_time = timer_visible_at_zoom && !cooldown_timer_visible;
            let has_timer_line = cooldown_timer_visible || show_dynamic_time;

            let static_name_bottom = static_name_bottom_bound(
                self.use_static_gpu_labels,
                self.static_show_names,
                ww,
                hh,
                cy,
                scale,
                static_tag_scale,
                static_name_scale,
            );
            let compact_bottom_y = cy + tag_size / 2.0;
            let mut content_bottom_y = compact_bottom_y;
            if has_timer_line {
                let stacked_total_h = tag_size + detail_size + time_size + line_gap * 2.0;
                let stacked_top_y = cy - stacked_total_h / 2.0;
                let mut time_y =
                    stacked_top_y + tag_size + line_gap + detail_size + line_gap + time_size / 2.0;
                if let Some(name_bottom_y) = static_name_bottom {
                    let min_time_y = name_bottom_y + time_size * 0.5 + line_gap * 0.8;
                    time_y = time_y.max(min_time_y);
                }
                let stacked_bottom_y = time_y + time_size / 2.0;
                content_bottom_y =
                    lerp_f32(compact_bottom_y, stacked_bottom_y, detail_layout_alpha);
            }
            if let Some(name_bottom_y) = static_name_bottom {
                content_bottom_y = content_bottom_y.max(name_bottom_y + line_gap * 0.6);
            }
            let cooldown_anchor_y =
                content_bottom_y + cooldown_size / 2.0 + lerp_f32(3.0, 4.0, detail_layout_alpha);

            // Keep resource icons on the same world-space sizing model as the text overlays.
            let icon_size_world = compute_resource_icon_size_world(icon_scale);
            let icon_gap_world = icon_size_world * 1.3;
            let icon_offset_world = lerp_f32(3.0, 4.0, detail_layout_alpha).max(0.0);
            let icon_y = if cooldown_timer_visible {
                cooldown_anchor_y + cooldown_size / 2.0 + icon_size_world / 2.0 + icon_offset_world
            } else {
                content_bottom_y + icon_size_world / 2.0 + icon_offset_world
            };
            let total_w = (icon_kinds.len() as f32 - 1.0) * icon_gap_world + icon_size_world;
            let mut dx = cx - total_w / 2.0;
            for kind in icon_kinds {
                let Some(uv) = renderer.uv_by_kind.get(&kind).copied() else {
                    continue;
                };
                renderer.instances_buf.push(IconInstance {
                    rect: [
                        dx,
                        icon_y - icon_size_world / 2.0,
                        icon_size_world,
                        icon_size_world,
                    ],
                    uv_rect: uv,
                    tint: [1.0, 1.0, 1.0, 1.0],
                });
                dx += icon_gap_world;
            }
        }

        let instances = renderer.instances_buf.as_slice();
        Self::upload_icon_buffer(
            &self.device,
            &self.queue,
            instances,
            &mut renderer.instance_buffer,
            &mut renderer.instance_count,
            &mut renderer.instance_capacity,
        );
        self.diag_icon_rebuilds = self.diag_icon_rebuilds.saturating_add(1);
        self.icon_dirty = false;
    }

    fn update_connection_vertices(&mut self, territories: &ClientTerritoryMap, scale: f64) {
        self.connection_vertices.clear();
        if !self.show_connections {
            self.connection_count = 0;
            self.connection_dirty = false;
            return;
        }

        let zoom_fade = smoothstep_f32(0.15, 0.45, scale as f32);
        if zoom_fade < 0.001 {
            self.connection_count = 0;
            self.connection_dirty = false;
            return;
        }

        self.connection_drawn_set.clear();
        for ct in territories.values() {
            let loc = &ct.territory.location;
            let name_hash = ct.name_hash;
            let ax = loc.midpoint_x() as f32;
            let ay = loc.midpoint_y() as f32;

            for conn_name in &ct.territory.connections {
                let Some(conn_ct) = territories.get(conn_name) else {
                    continue;
                };
                let conn_hash = conn_ct.name_hash;
                let edge = if name_hash < conn_hash {
                    (name_hash, conn_hash)
                } else {
                    (conn_hash, name_hash)
                };
                if !self.connection_drawn_set.insert(edge) {
                    continue;
                }
                let conn_loc = &conn_ct.territory.location;
                let bx = conn_loc.midpoint_x() as f32;
                let by = conn_loc.midpoint_y() as f32;
                let dx = bx - ax;
                let dy = by - ay;
                let len_sq = dx * dx + dy * dy;
                if len_sq <= f32::EPSILON {
                    continue;
                }
                let inv_len = len_sq.sqrt().recip();
                let nx = -dy * inv_len;
                let ny = dx * inv_len;
                let world_per_px = (1.0 / (scale as f32).max(0.05)).min(24.0);
                let opacity_scale = self.connection_opacity_scale.max(0.0);
                let thickness_scale = self.connection_thickness_scale.max(0.2);

                let color = if self.bold_connections {
                    let (cr, cg, cb) = ct.guild_color;
                    let lum = 0.299 * cr as f64 + 0.587 * cg as f64 + 0.114 * cb as f64;
                    let dark_boost = (1.0 - lum / 255.0).clamp(0.0, 1.0);
                    let brighten_factor = 1.4 + dark_boost * 0.8;
                    let alpha = (0.35 + dark_boost * 0.20) as f32 * zoom_fade * opacity_scale;
                    let (r, g, b) = brighten(cr, cg, cb, brighten_factor);
                    [
                        r as f32 / 255.0,
                        g as f32 / 255.0,
                        b as f32 / 255.0,
                        alpha.clamp(0.0, 1.0),
                    ]
                } else {
                    [1.0, 1.0, 1.0, 0.16 * zoom_fade * opacity_scale]
                };

                let thickness_steps = if self.bold_connections {
                    CONNECTION_LINE_STEPS_BOLD
                } else {
                    CONNECTION_LINE_STEPS_NORMAL
                };
                for &(offset_px, alpha_scale) in thickness_steps {
                    let offset_world = offset_px * thickness_scale * world_per_px;
                    let ox = nx * offset_world;
                    let oy = ny * offset_world;
                    let mut line_color = color;
                    line_color[3] = (color[3] * alpha_scale).clamp(0.0, 1.0);
                    self.connection_vertices.push(ConnectionVertex {
                        world_pos: [ax + ox, ay + oy],
                        color: line_color,
                    });
                    self.connection_vertices.push(ConnectionVertex {
                        world_pos: [bx + ox, by + oy],
                        color: line_color,
                    });
                }
            }
        }

        self.connection_count = self.connection_vertices.len() as u32;
        if self.connection_count > self.connection_capacity {
            self.connection_capacity = self.connection_count.next_power_of_two();
            self.connection_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("connection-vertex-buf"),
                size: (self.connection_capacity as u64)
                    * std::mem::size_of::<ConnectionVertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.connection_vertices.is_empty() {
            self.queue.write_buffer(
                &self.connection_buffer,
                0,
                bytemuck::cast_slice(&self.connection_vertices),
            );
        }
        self.connection_dirty = false;
    }

    /// Render a full frame. Returns true if animations are active.
    pub fn render(&mut self, frame: RenderFrameInput<'_>) -> bool {
        let RenderFrameInput {
            vp,
            territories,
            hovered,
            selected,
            tiles,
            world_bounds,
            now,
            reference_time_secs,
            interaction_active,
            icons,
            show_minimap,
            history_mode,
            heat_mode_enabled,
            heat_entries,
            heat_max_take_count,
        } = frame;
        let frame_start_ms = now;
        let mut draw_calls: u32 = 0;
        let mut tile_draw_calls: u32 = 0;
        let mut bytes_uploaded: u64 = 0;
        let minimap_world_bounds = self.tile_world_bounds.or(world_bounds);

        // CSS pixel dimensions for viewport/culling (shaders work in CSS space)
        let w = self.width as f32 / self.dpr;
        let h = self.height as f32 / self.dpr;

        // Update viewport uniform
        self.queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::cast_slice(&[ViewportUniform {
                offset: [vp.offset_x as f32, vp.offset_y as f32],
                scale: vp.scale as f32,
                time: ((now - self.start_time_ms) / 1000.0) as f32,
                resolution: [w, h],
                _pad1: [
                    (reference_time_secs as f64 - self.start_time_ms / 1000.0) as f32,
                    0.0,
                ],
            }]),
        );
        bytes_uploaded += std::mem::size_of::<ViewportUniform>() as u64;

        let mut did_static_rebuild = false;
        let mut did_dynamic_rebuild = false;
        let mut did_icon_rebuild = false;

        // Update instance buffer only when state has changed.
        // Animation color interpolation is GPU-side — no per-frame rebuild needed.
        if self.instance_dirty {
            self.update_instances(
                territories,
                hovered,
                selected,
                now,
                self.thick_cooldown_borders,
                heat_mode_enabled,
                heat_entries,
                heat_max_take_count,
            );
            bytes_uploaded +=
                (self.instance_count as u64) * std::mem::size_of::<TerritoryInstance>() as u64;
            self.instance_dirty = false;
        }
        if self.connection_dirty {
            self.update_connection_vertices(territories, vp.scale);
            bytes_uploaded +=
                (self.connection_count as u64) * std::mem::size_of::<ConnectionVertex>() as u64;
        }

        if (self.use_full_gpu_text || self.use_static_gpu_labels) && self.text_renderer.is_none() {
            if self.ensure_text_renderer() {
                self.static_text_dirty = true;
                self.dynamic_text_dirty = true;
            } else {
                // Fail closed: map rendering requires the GPU text pipeline.
                return false;
            }
        }
        if self.use_full_gpu_text
            && let Some(icon_set) = icons.as_ref()
            && self.icon_renderer.is_none()
        {
            if self.ensure_icon_renderer(icon_set) {
                self.icon_dirty = true;
            }
        }

        let static_zoom_bucket = Self::static_zoom_bucket(vp.scale);
        let timer_visible_at_zoom = vp.scale >= DYNAMIC_TIMER_VISIBILITY_MIN_SCALE;
        if timer_visible_at_zoom != self.dynamic_timer_visible_at_zoom {
            self.dynamic_timer_visible_at_zoom = timer_visible_at_zoom;
            // Crossing the timer visibility threshold changes text/icon layout even when
            // zoom buckets remain unchanged; force an immediate refit.
            self.static_text_dirty = true;
            self.dynamic_text_dirty = true;
            self.icon_dirty = true;
        }
        if static_zoom_bucket != self.static_zoom_bucket {
            self.static_zoom_bucket = static_zoom_bucket;
            self.static_text_dirty = true;
        }

        if self.static_text_dirty {
            self.update_static_text_instances(territories, vp);
            if let Some(text_renderer) = self.text_renderer.as_ref() {
                let per = std::mem::size_of::<TextInstance>() as u64;
                bytes_uploaded += (text_renderer.static_fill_count as u64
                    + text_renderer.static_halo_count as u64)
                    * per;
            }
            did_static_rebuild = true;
        }

        let zoom_bucket = Self::dynamic_zoom_bucket(vp.scale);
        if zoom_bucket != self.dynamic_zoom_bucket {
            self.dynamic_zoom_bucket = zoom_bucket;
            self.dynamic_text_dirty = true;
            self.icon_dirty = true;
            self.connection_dirty = true;
        }
        if reference_time_secs != self.dynamic_reference_time_secs {
            let prev_reference = self.dynamic_reference_time_secs;
            self.dynamic_reference_time_secs = reference_time_secs;
            let stepped_back = prev_reference != i64::MIN && reference_time_secs < prev_reference;
            let crossed_scheduled_boundary = self.dynamic_next_update_secs == i64::MIN
                || reference_time_secs >= self.dynamic_next_update_secs;
            if stepped_back || crossed_scheduled_boundary {
                if interaction_active {
                    self.dynamic_refresh_deferred = true;
                } else {
                    self.dynamic_text_dirty = true;
                    self.dynamic_refresh_deferred = false;
                    // Icons can move when cooldown line appears/disappears.
                    if self.dynamic_show_countdown {
                        self.icon_dirty = true;
                    }
                }
            }
        }
        if !interaction_active && self.dynamic_refresh_deferred {
            self.dynamic_text_dirty = true;
            self.dynamic_refresh_deferred = false;
            if self.dynamic_show_countdown {
                self.icon_dirty = true;
            }
        }
        if self.use_full_gpu_text && self.dynamic_text_dirty {
            self.update_dynamic_text_instances(territories, vp, reference_time_secs);
            if let Some(text_renderer) = self.text_renderer.as_ref() {
                let per = std::mem::size_of::<TextInstance>() as u64;
                bytes_uploaded += (text_renderer.dynamic_fill_count as u64
                    + text_renderer.dynamic_halo_count as u64)
                    * per;
            }
            did_dynamic_rebuild = true;
        }
        if self.use_full_gpu_text && self.icon_dirty && self.icon_renderer.is_some() {
            self.update_icon_instances(territories, vp, reference_time_secs);
            if let Some(icon_renderer) = self.icon_renderer.as_ref() {
                bytes_uploaded += (icon_renderer.instance_count as u64)
                    * std::mem::size_of::<IconInstance>() as u64;
            }
            did_icon_rebuild = true;
        }

        let pan_only = (vp.scale - self.diag_last_vp.2).abs() < 0.000001
            && ((vp.offset_x - self.diag_last_vp.0).abs() > 0.001
                || (vp.offset_y - self.diag_last_vp.1).abs() > 0.001);
        if pan_only && !did_static_rebuild && !did_dynamic_rebuild && !did_icon_rebuild {
            self.diag_pan_only_zero_rebuild_frames =
                self.diag_pan_only_zero_rebuild_frames.saturating_add(1);
        }
        self.diag_last_vp = (vp.offset_x, vp.offset_y, vp.scale);

        if self.diag_console_logging
            && (did_static_rebuild || did_dynamic_rebuild || did_icon_rebuild)
        {
            web_sys::console::log_1(
                &format!(
                    "gpu-diag static_rebuilds={} dynamic_rebuilds={} icon_rebuilds={} pan_zero_rebuild_frames={}",
                    self.diag_static_rebuilds,
                    self.diag_dynamic_rebuilds,
                    self.diag_icon_rebuilds,
                    self.diag_pan_only_zero_rebuild_frames
                )
                .into(),
            );
            self.diag_static_rebuilds = 0;
            self.diag_dynamic_rebuilds = 0;
            self.diag_icon_rebuilds = 0;
            self.diag_pan_only_zero_rebuild_frames = 0;
        }

        // Pre-compute glow uniforms and write buffers BEFORE the render pass
        // to avoid pipeline stalls from mid-pass buffer writes on WebGL2/glow.
        let mut draw_sel_glow = false;
        let mut draw_hov_glow = false;

        if let Some(sel_name) = selected {
            if let Some(ct) = territories.get(sel_name) {
                let loc = &ct.territory.location;
                let (r, g, b) = ct.guild_color;
                let expand_world = 8.0 / vp.scale as f32;
                self.queue.write_buffer(
                    &self.glow_buffer_sel,
                    0,
                    bytemuck::cast_slice(&[GlowUniform {
                        rect: [
                            loc.left() as f32 - expand_world,
                            loc.top() as f32 - expand_world,
                            loc.width() as f32 + expand_world * 2.0,
                            loc.height() as f32 + expand_world * 2.0,
                        ],
                        glow_color: [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.35],
                        expand: 6.0,
                        falloff: 0.03,
                        ring_width: 1.5,
                        fill_tint_alpha: 0.02,
                        fill_tint_rgb: [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
                        _pad: 0.0,
                    }]),
                );
                bytes_uploaded += std::mem::size_of::<GlowUniform>() as u64;
                draw_sel_glow = true;
            }
        }

        if let Some(hov_name) = hovered {
            if selected.as_deref() != Some(hov_name.as_str()) {
                if let Some(ct) = territories.get(hov_name) {
                    let loc = &ct.territory.location;
                    let (r, g, b) = ct.guild_color;
                    let expand_world = 5.0 / vp.scale as f32;
                    self.queue.write_buffer(
                        &self.glow_buffer_hov,
                        0,
                        bytemuck::cast_slice(&[GlowUniform {
                            rect: [
                                loc.left() as f32 - expand_world,
                                loc.top() as f32 - expand_world,
                                loc.width() as f32 + expand_world * 2.0,
                                loc.height() as f32 + expand_world * 2.0,
                            ],
                            glow_color: [
                                r as f32 / 255.0,
                                g as f32 / 255.0,
                                b as f32 / 255.0,
                                0.25,
                            ],
                            expand: 5.0,
                            falloff: 0.035,
                            ring_width: 1.0,
                            fill_tint_alpha: 0.0,
                            fill_tint_rgb: [0.0, 0.0, 0.0],
                            _pad: 0.0,
                        }]),
                    );
                    bytes_uploaded += std::mem::size_of::<GlowUniform>() as u64;
                    draw_hov_glow = true;
                }
            }
        }

        // Get surface texture
        let output = match self.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.surface_config);
                return false;
            }
            Err(_) => return false,
        };

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.047,
                            g: 0.055,
                            b: 0.090,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });

            // Draw tiles
            if !tiles.is_empty() {
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                for tile in tiles {
                    let Some(tile_tex) = self.tile_textures.get(&tile.id) else {
                        continue;
                    };

                    let [x1, z1, tw, th] = tile_tex.rect;
                    let x2 = x1 + tw;
                    let z2 = z1 + th;

                    // World bounds culling
                    if let Some((bx1, by1, bx2, by2)) = world_bounds {
                        let margin = 300.0;
                        if (x2 as f64) < bx1 - margin
                            || (x1 as f64) > bx2 + margin
                            || (z2 as f64) < by1 - margin
                            || (z1 as f64) > by2 + margin
                        {
                            continue;
                        }
                    }

                    // Frustum cull + screen-size cull (skip tiny tiles)
                    let sx = x1 * vp.scale as f32 + vp.offset_x as f32;
                    let sy = z1 * vp.scale as f32 + vp.offset_y as f32;
                    let sw = tw * vp.scale as f32;
                    let sh = th * vp.scale as f32;
                    if sx + sw < 0.0 || sy + sh < 0.0 || sx > w || sy > h {
                        continue;
                    }
                    // Skip tiles smaller than 4px on screen — saves draw calls
                    // and texture bandwidth at extreme zoom-out
                    if sw < 4.0 || sh < 4.0 {
                        continue;
                    }

                    pass.set_bind_group(1, &tile_tex.bind_group, &[]);
                    pass.draw_indexed(0..6, 0, 0..1);
                    draw_calls = draw_calls.saturating_add(1);
                    tile_draw_calls = tile_draw_calls.saturating_add(1);
                }
            }

            // Draw territory fills + borders (instanced)
            if self.instance_count > 0 {
                pass.set_pipeline(&self.territory_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..6, 0, 0..self.instance_count);
                draw_calls = draw_calls.saturating_add(1);
            }

            if self.connection_count > 0 {
                pass.set_pipeline(&self.connection_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.connection_buffer.slice(..));
                pass.draw(0..self.connection_count, 0..1);
                draw_calls = draw_calls.saturating_add(1);
            }

            // Glow draws — uniforms already written before pass to avoid
            // pipeline stalls from mid-pass buffer writes on WebGL2/glow
            if draw_sel_glow || draw_hov_glow {
                pass.set_pipeline(&self.glow_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                if draw_sel_glow {
                    pass.set_bind_group(1, &self.glow_bind_group_sel, &[]);
                    pass.draw_indexed(0..6, 0, 0..1);
                    draw_calls = draw_calls.saturating_add(1);
                }

                if draw_hov_glow {
                    pass.set_bind_group(1, &self.glow_bind_group_hov, &[]);
                    pass.draw_indexed(0..6, 0, 0..1);
                    draw_calls = draw_calls.saturating_add(1);
                }
            }

            if self.use_static_gpu_labels
                && let Some(text_renderer) = self.text_renderer.as_ref()
            {
                pass.set_pipeline(&text_renderer.pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                if text_renderer.static_halo_count > 0 {
                    pass.set_bind_group(1, &text_renderer.halo_bind_group, &[]);
                    pass.set_vertex_buffer(1, text_renderer.static_halo_buffer.slice(..));
                    pass.draw_indexed(0..6, 0, 0..text_renderer.static_halo_count);
                    draw_calls = draw_calls.saturating_add(1);
                }
                if text_renderer.static_fill_count > 0 {
                    pass.set_bind_group(1, &text_renderer.fill_bind_group, &[]);
                    pass.set_vertex_buffer(1, text_renderer.static_fill_buffer.slice(..));
                    pass.draw_indexed(0..6, 0, 0..text_renderer.static_fill_count);
                    draw_calls = draw_calls.saturating_add(1);
                }

                if self.use_full_gpu_text && text_renderer.dynamic_halo_count > 0 {
                    pass.set_bind_group(1, &text_renderer.halo_bind_group, &[]);
                    pass.set_vertex_buffer(1, text_renderer.dynamic_halo_buffer.slice(..));
                    pass.draw_indexed(0..6, 0, 0..text_renderer.dynamic_halo_count);
                    draw_calls = draw_calls.saturating_add(1);
                }
                if self.use_full_gpu_text && text_renderer.dynamic_fill_count > 0 {
                    pass.set_bind_group(1, &text_renderer.fill_bind_group, &[]);
                    pass.set_vertex_buffer(1, text_renderer.dynamic_fill_buffer.slice(..));
                    pass.draw_indexed(0..6, 0, 0..text_renderer.dynamic_fill_count);
                    draw_calls = draw_calls.saturating_add(1);
                }
            }

            if self.use_full_gpu_text
                && let Some(icon_renderer) = self.icon_renderer.as_ref()
                && icon_renderer.instance_count > 0
            {
                pass.set_pipeline(&icon_renderer.pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_bind_group(1, &icon_renderer.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, icon_renderer.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..6, 0, 0..icon_renderer.instance_count);
                draw_calls = draw_calls.saturating_add(1);
            }
        }

        if show_minimap {
            let minimap_bottom = if history_mode {
                MINIMAP_HISTORY_BOTTOM
            } else {
                MINIMAP_MARGIN
            };
            let minimap_x = MINIMAP_MARGIN;
            let minimap_y = (h - MINIMAP_H - minimap_bottom).max(0.0);
            let scissor_x = (minimap_x * self.dpr).floor().max(0.0) as u32;
            let scissor_y = (minimap_y * self.dpr).floor().max(0.0) as u32;
            let scissor_w =
                ((MINIMAP_W * self.dpr).ceil() as u32).min(self.width.saturating_sub(scissor_x));
            let scissor_h =
                ((MINIMAP_H * self.dpr).ceil() as u32).min(self.height.saturating_sub(scissor_y));

            if scissor_w > 0 && scissor_h > 0 {
                let (world_min_x, world_min_y, world_max_x, world_max_y) =
                    minimap_world_bounds.unwrap_or(MINIMAP_DEFAULT_WORLD_BOUNDS);
                let world_w = ((world_max_x - world_min_x).max(1.0)) as f32;
                let world_h = ((world_max_y - world_min_y).max(1.0)) as f32;
                let minimap_scale = (MINIMAP_W / world_w).min(MINIMAP_H / world_h);
                let used_w = world_w * minimap_scale;
                let used_h = world_h * minimap_scale;
                let minimap_offset_x =
                    minimap_x + (MINIMAP_W - used_w) * 0.5 - (world_min_x as f32) * minimap_scale;
                let minimap_offset_y =
                    minimap_y + (MINIMAP_H - used_h) * 0.5 - (world_min_y as f32) * minimap_scale;

                self.queue.write_buffer(
                    &self.minimap_viewport_buffer,
                    0,
                    bytemuck::cast_slice(&[ViewportUniform {
                        offset: [minimap_offset_x, minimap_offset_y],
                        scale: minimap_scale,
                        time: ((now - self.start_time_ms) / 1000.0) as f32,
                        resolution: [w, h],
                        _pad1: [
                            (reference_time_secs as f64 - self.start_time_ms / 1000.0) as f32,
                            0.0,
                        ],
                    }]),
                );
                bytes_uploaded += std::mem::size_of::<ViewportUniform>() as u64;

                let (tl_wx, tl_wy) = vp.screen_to_world(0.0, 0.0);
                let (br_wx, br_wy) = vp.screen_to_world(w as f64, h as f64);
                let left = tl_wx.min(br_wx) as f32;
                let right = tl_wx.max(br_wx) as f32;
                let top = tl_wy.min(br_wy) as f32;
                let bottom = tl_wy.max(br_wy) as f32;
                let world_min_x_f = world_min_x as f32;
                let world_min_y_f = world_min_y as f32;
                let world_max_x_f = world_max_x as f32;
                let world_max_y_f = world_max_y as f32;
                let left = left.clamp(world_min_x_f, world_max_x_f);
                let right = right.clamp(world_min_x_f, world_max_x_f);
                let top = top.clamp(world_min_y_f, world_max_y_f);
                let bottom = bottom.clamp(world_min_y_f, world_max_y_f);
                let color = [245.0 / 255.0, 197.0 / 255.0, 66.0 / 255.0, 0.95];
                let indicator_vertices = [
                    ConnectionVertex {
                        world_pos: [left, top],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [right, top],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [right, top],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [right, bottom],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [right, bottom],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [left, bottom],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [left, bottom],
                        color,
                    },
                    ConnectionVertex {
                        world_pos: [left, top],
                        color,
                    },
                ];
                let indicator_count = if right > left && bottom > top {
                    8u32
                } else {
                    0u32
                };
                if indicator_count > self.minimap_indicator_capacity {
                    self.minimap_indicator_capacity = indicator_count.next_power_of_two();
                    self.minimap_indicator_buffer =
                        self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("minimap-indicator-vertex-buf"),
                            size: (self.minimap_indicator_capacity as u64)
                                * std::mem::size_of::<ConnectionVertex>() as u64,
                            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                }
                if indicator_count > 0 {
                    self.queue.write_buffer(
                        &self.minimap_indicator_buffer,
                        0,
                        bytemuck::cast_slice(&indicator_vertices),
                    );
                    bytes_uploaded +=
                        (indicator_count as u64) * std::mem::size_of::<ConnectionVertex>() as u64;
                }

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("minimap-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);

                // Minimap background fill
                {
                    let bg_color = [19.0 / 255.0, 22.0 / 255.0, 31.0 / 255.0, 0.88_f32];
                    let (wmx, wmy, wmxx, wmxy) =
                        minimap_world_bounds.unwrap_or(MINIMAP_DEFAULT_WORLD_BOUNDS);
                    let pad = 200.0_f32;
                    let bg_vertices = [
                        ConnectionVertex {
                            world_pos: [wmx as f32 - pad, wmy as f32 - pad],
                            color: bg_color,
                        },
                        ConnectionVertex {
                            world_pos: [wmxx as f32 + pad, wmy as f32 - pad],
                            color: bg_color,
                        },
                        ConnectionVertex {
                            world_pos: [wmxx as f32 + pad, wmxy as f32 + pad],
                            color: bg_color,
                        },
                        ConnectionVertex {
                            world_pos: [wmx as f32 - pad, wmy as f32 - pad],
                            color: bg_color,
                        },
                        ConnectionVertex {
                            world_pos: [wmxx as f32 + pad, wmxy as f32 + pad],
                            color: bg_color,
                        },
                        ConnectionVertex {
                            world_pos: [wmx as f32 - pad, wmxy as f32 + pad],
                            color: bg_color,
                        },
                    ];
                    self.queue.write_buffer(
                        &self.minimap_bg_buffer,
                        0,
                        bytemuck::cast_slice(&bg_vertices),
                    );
                    pass.set_pipeline(&self.connection_fill_pipeline);
                    pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.minimap_bg_buffer.slice(..));
                    pass.draw(0..6, 0..1);
                    draw_calls = draw_calls.saturating_add(1);
                }

                if !tiles.is_empty() {
                    pass.set_pipeline(&self.tile_pipeline);
                    pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                    pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                    for tile in tiles {
                        let Some(tile_tex) = self.tile_textures.get(&tile.id) else {
                            continue;
                        };
                        pass.set_bind_group(1, &tile_tex.bind_group, &[]);
                        pass.draw_indexed(0..6, 0, 0..1);
                        draw_calls = draw_calls.saturating_add(1);
                        tile_draw_calls = tile_draw_calls.saturating_add(1);
                    }
                }

                if self.instance_count > 0 {
                    pass.set_pipeline(&self.territory_pipeline);
                    pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                    pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                    pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                    pass.draw_indexed(0..6, 0, 0..self.instance_count);
                    draw_calls = draw_calls.saturating_add(1);
                }

                if self.connection_count > 0 {
                    pass.set_pipeline(&self.connection_pipeline);
                    pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.connection_buffer.slice(..));
                    pass.draw(0..self.connection_count, 0..1);
                    draw_calls = draw_calls.saturating_add(1);
                }

                pass.set_pipeline(&self.connection_pipeline);
                pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.minimap_indicator_buffer.slice(..));
                pass.draw(0..indicator_count, 0..1);
                draw_calls = draw_calls.saturating_add(1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        let frame_cpu_ms = (js_sys::Date::now() - frame_start_ms).max(0.0);
        let fps_estimate = if self.last_render_time_ms > 0.0 {
            let dt = (frame_start_ms - self.last_render_time_ms).max(0.0001);
            1000.0 / dt
        } else {
            0.0
        };
        self.last_render_time_ms = frame_start_ms;
        let text_instances = self
            .text_renderer
            .as_ref()
            .map(|text| {
                text.static_fill_count
                    + text.static_halo_count
                    + text.dynamic_fill_count
                    + text.dynamic_halo_count
            })
            .unwrap_or(0);
        self.frame_metrics = FrameMetrics {
            frame_cpu_ms,
            draw_calls,
            tile_draw_calls,
            bytes_uploaded,
            resolution_scale: self.dpr,
            territory_instances: self.instance_count,
            text_instances,
            fps_estimate,
        };

        // Check if any animations are still running (cached during instance rebuild)
        now < self.max_anim_end_ms
    }
}
