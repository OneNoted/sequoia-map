//! The wgpu/WebGL2 map renderer.
//!
//! It owns every GPU resource: pipelines, tile textures, the glyph and icon atlases, the
//! cached instance buffers and the minimap terrain image. What to rebuild each frame is
//! decided by the scene planner and passed in as a [`Rebuild`].

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};
use wgpu::util::DeviceExt;

use sequoia_map_engine::claim_labels::{
    self, CLAIM_LABEL_LETTER_SPACING_EM, build_claim_clusters, claim_label_zoom_active,
    select_claim_label_candidates,
};
use sequoia_map_engine::colors::{brighten, heat_color_for_count};
use sequoia_map_engine::defense::defense_tier_overlay_data;
use sequoia_map_engine::icon_atlas::ICON_COUNT;
use sequoia_map_engine::label_layout::{
    IconKind, abbreviate_name, compute_label_layout_metrics, cooldown_color,
    dynamic_label_next_update_age, dynamic_text_state, resource_icon_sequence,
    resource_icons_drawable, write_age, write_age_compound,
};
use sequoia_map_engine::minimap::MinimapLayout;
use sequoia_map_engine::overlay_sizing::{
    STATIC_NAME_BASELINE_GAP_MULTIPLIER, STATIC_NAME_MIN_RENDERED_PX, compute_dynamic_label_sizing,
    compute_far_zoom_tag_sizing, compute_resource_icon_center_y_world,
    compute_resource_icon_label_lift_world, compute_resource_icon_size_world,
    compute_static_label_sizing, compute_territory_ornament_sizing,
    compute_territory_ornament_tint, static_name_bottom_bound,
};
use sequoia_map_engine::scene::{
    LABEL_VISIBILITY_MIN_SCALE, NextRefresh, Rebuild, TIMER_VISIBILITY_MIN_SCALE,
};
use sequoia_map_engine::settings::{NameColor, RenderSettings};
use sequoia_map_engine::territory::{ClientTerritoryMap, is_sequoia_guild, is_unclaimed_guild};
use sequoia_map_engine::time_format::write_hms;
use sequoia_map_engine::viewport::Viewport;
use sequoia_shared::TreasuryLevel;
use sequoia_shared::colors::hsl_to_rgb;
use sequoia_shared::territory::Resources;

use crate::frame::{Frame, FrameMetrics, FrameOutcome, RenderCapabilities};
use crate::icons::ResourceAtlas;
use crate::tiles::{LoadedTile, TileQuality};

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
const STATIC_NAME_FILL_ALPHA_MULTIPLIER: f32 = 0.84;
const STATIC_NAME_HALO_ALPHA_MULTIPLIER: f32 = 0.88;

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

struct GlyphAtlas {
    bind_group_layout: wgpu::BindGroupLayout,
    fill_bind_group: wgpu::BindGroup,
    halo_bind_group: wgpu::BindGroup,
    glyphs: HashMap<char, GlyphMeta>,
    kerning: HashMap<u32, f32>,
    line_height: f32,
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
    default_ornament_uv: [f32; 4],
    default_ornament_aspect: f32,
    sequoia_ornament_uv: [f32; 4],
    sequoia_ornament_aspect: f32,
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
const STATIC_TAG_LETTER_SPACING_EM: f32 = 0.07;
const STATIC_NAME_LETTER_SPACING_EM: f32 = 0.057;
const STATIC_TAG_MIN_WIDTH_WORLD: f32 = 88.0;
const STATIC_NAME_MIN_WIDTH_WORLD: f32 = 176.0;
const STATIC_TAG_MIN_RENDERED_PX: f32 = 13.5;
const DYNAMIC_TIME_LETTER_SPACING_EM: f32 = 0.035;
const DYNAMIC_COOLDOWN_LETTER_SPACING_EM_MIN: f32 = 0.0035;
const DYNAMIC_TIME_MIN_RENDERED_PX: f32 = 11.5;
const DYNAMIC_COOLDOWN_MIN_RENDERED_PX: f32 = 12.0;
/// A captured territory is fresh (on cooldown) for this long.
const FRESH_TERRITORY_SECS: i64 = 600;
const HQ_CROWN_SIZE_MULTIPLIER: f32 = 1.75;
const HQ_CROWN_FAR_BOX_FRACTION: f32 = 0.90;
const HQ_CROWN_FAR_MAX_RENDERED_PX: f32 = 96.0;
const HQ_CROWN_EXPANDED_MAX_SCALE: f32 = 1.15;
const HQ_CROWN_NORMAL_TOP_PADDING_PX: f32 = 4.0;
const HQ_CROWN_NORMAL_SIDE_PADDING_PX: f32 = 4.0;
const HQ_CROWN_NORMAL_LABEL_GAP_PX: f32 = 3.0;
const HQ_CROWN_NORMAL_MAX_HEIGHT_FRACTION: f32 = 0.24;
const HQ_CROWN_NORMAL_MIN_RENDERED_PX: f32 = 12.0;
const ORNAMENT_MIN_RENDERED_PX: f32 = 1.0;
const ORNAMENT_UV_PADDING_PX: u32 = 2;
const SEQUOIA_ORNAMENT_FALLBACK_GOLD: [u8; 3] = [245, 197, 66];

#[derive(Clone, Copy, Debug, PartialEq)]
struct HqCrownLayout {
    size_world: f32,
    center_y: f32,
    label_clear_y: f32,
}

#[inline]
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[inline]
fn resource_icons_visible_for_territory(
    show_resource_icons: bool,
    sw: f32,
    sh: f32,
    detail_layout_alpha: f32,
    resources: &Resources,
) -> bool {
    show_resource_icons
        && sw > 55.0
        && sh > 35.0
        && detail_layout_alpha > 0.001
        && resource_icons_drawable(resources)
}

#[inline]
fn hq_crown_expanded_at_zoom(px_per_world: f32) -> bool {
    px_per_world <= HQ_CROWN_EXPANDED_MAX_SCALE
}

#[inline]
fn hq_label_max_width_world(territory_width: f32, padding_world: f32) -> f32 {
    (territory_width - padding_world).max(1.0)
}

fn hq_normal_crown_layout(
    territory_top: f32,
    territory_width: f32,
    territory_height: f32,
    px_per_world: f32,
    preferred_size_world: f32,
) -> Option<HqCrownLayout> {
    let px_per_world = px_per_world.max(0.0001);
    let top_padding_world = HQ_CROWN_NORMAL_TOP_PADDING_PX / px_per_world;
    let side_padding_world = HQ_CROWN_NORMAL_SIDE_PADDING_PX / px_per_world;
    let label_gap_world = HQ_CROWN_NORMAL_LABEL_GAP_PX / px_per_world;
    let max_width = (territory_width - side_padding_world * 2.0).max(0.0);
    let max_height = (territory_height * HQ_CROWN_NORMAL_MAX_HEIGHT_FRACTION).max(0.0);
    let max_size = max_width.min(max_height);
    if max_size <= 0.0 {
        return None;
    }

    let min_visible_world = HQ_CROWN_NORMAL_MIN_RENDERED_PX / px_per_world;
    let min_size = min_visible_world.min(max_size);
    let size_world = preferred_size_world.clamp(min_size, max_size);
    let top_y = territory_top + top_padding_world;
    let bottom_y = top_y + size_world;

    Some(HqCrownLayout {
        size_world,
        center_y: top_y + size_world * 0.5,
        label_clear_y: bottom_y + label_gap_world,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Explicit geometry and font metrics keep the crown/tag layout calculation stateless."
)]
fn hq_normal_static_tag_y(
    territory_top: f32,
    territory_width: f32,
    territory_height: f32,
    center_y: f32,
    px_per_world: f32,
    tag_size: f32,
    detail_size: f32,
    detail_layout_alpha: f32,
    label_lift: f32,
) -> f32 {
    let base_y = lerp_f32(
        center_y,
        center_y - (detail_size + 1.0) * 0.45,
        detail_layout_alpha,
    ) - label_lift;
    let preferred_crown_size = tag_size * HQ_CROWN_SIZE_MULTIPLIER;
    hq_normal_crown_layout(
        territory_top,
        territory_width,
        territory_height,
        px_per_world,
        preferred_crown_size,
    )
    .map(|layout| base_y.max(layout.label_clear_y + tag_size * 0.5))
    .unwrap_or(base_y)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Explicit geometry and display settings keep this label bounds calculation stateless."
)]
fn hq_normal_static_label_bottom_bound(
    territory_top: f32,
    territory_width: f32,
    territory_height: f32,
    center_y: f32,
    px_per_world: f32,
    static_show_names: bool,
    static_tag_scale: f32,
    static_name_scale: f32,
    resource_icons_visible: bool,
) -> Option<f32> {
    let sizing = compute_static_label_sizing(territory_width, territory_height)?;
    let detail_layout_alpha = sizing.detail_layout_alpha;
    let tag_size = sizing.tag_size * static_tag_scale;
    let detail_size = sizing.detail_size * static_name_scale;
    let label_lift = compute_resource_icon_label_lift_world(
        territory_height,
        detail_layout_alpha,
        resource_icons_visible,
    );
    let tag_visible = tag_size * px_per_world >= STATIC_TAG_MIN_RENDERED_PX;
    let name_visible = static_show_names
        && detail_layout_alpha > 0.02
        && detail_size * px_per_world >= STATIC_NAME_MIN_RENDERED_PX;
    let tag_y = hq_normal_static_tag_y(
        territory_top,
        territory_width,
        territory_height,
        center_y,
        px_per_world,
        tag_size,
        detail_size,
        detail_layout_alpha,
        label_lift,
    );
    let mut bottom_y = tag_visible.then_some(tag_y + tag_size * 0.5);
    if name_visible {
        let name_y = tag_y + tag_size * 0.5 + detail_size * STATIC_NAME_BASELINE_GAP_MULTIPLIER;
        bottom_y = Some(bottom_y.map_or(name_y + detail_size * 0.5, |bottom| {
            bottom.max(name_y + detail_size * 0.5)
        }));
    }
    bottom_y
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
fn ornament_mask_alpha(r: u8, g: u8, b: u8, src_a: u8) -> u8 {
    let lum = u16::from(r.max(g).max(b));
    ((lum * u16::from(src_a) + 127) / 255) as u8
}

#[inline]
fn is_sequoia_ornament_gold_sample(r: u8, g: u8, b: u8, alpha: u8) -> bool {
    if alpha <= 24 {
        return false;
    }
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let spread = maxc - minc;
    r >= 110 && g >= 80 && r > g && g > b.saturating_add(6) && spread >= 20
}

#[inline]
fn is_sequoia_ornament_neutral_highlight(r: u8, g: u8, b: u8, alpha: u8) -> bool {
    if alpha <= 24 {
        return false;
    }
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let spread = maxc - minc;
    maxc >= 150 && spread <= 36
}

fn derive_sequoia_ornament_gold(
    pixels: &[u8],
    atlas_w: u32,
    slot_x: u32,
    slot_w: u32,
    slot_h: u32,
) -> [u8; 3] {
    let mut sum_r = 0u64;
    let mut sum_g = 0u64;
    let mut sum_b = 0u64;
    let mut weight_total = 0u64;

    for y in 0..slot_h {
        for x in 0..slot_w {
            let atlas_px_x = slot_x + x;
            let idx = ((y * atlas_w + atlas_px_x) * 4) as usize;
            let r = pixels[idx];
            let g = pixels[idx + 1];
            let b = pixels[idx + 2];
            let src_a = pixels[idx + 3];
            let alpha = ornament_mask_alpha(r, g, b, src_a);
            if !is_sequoia_ornament_gold_sample(r, g, b, alpha) {
                continue;
            }
            let weight = u64::from(alpha) * u64::from(r.max(g).max(b));
            sum_r += u64::from(r) * weight;
            sum_g += u64::from(g) * weight;
            sum_b += u64::from(b) * weight;
            weight_total += weight;
        }
    }

    if weight_total == 0 {
        return SEQUOIA_ORNAMENT_FALLBACK_GOLD;
    }

    [
        (sum_r / weight_total) as u8,
        (sum_g / weight_total) as u8,
        (sum_b / weight_total) as u8,
    ]
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

fn set_claim_label_debug(scale: f64, active: bool, cluster_count: usize, rendered_count: usize) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let payload = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        payload.as_ref(),
        &wasm_bindgen::JsValue::from_str("scale"),
        &wasm_bindgen::JsValue::from_f64(scale),
    );
    let _ = js_sys::Reflect::set(
        payload.as_ref(),
        &wasm_bindgen::JsValue::from_str("active"),
        &wasm_bindgen::JsValue::from_bool(active),
    );
    let _ = js_sys::Reflect::set(
        payload.as_ref(),
        &wasm_bindgen::JsValue::from_str("clusterCount"),
        &wasm_bindgen::JsValue::from_f64(cluster_count as f64),
    );
    let _ = js_sys::Reflect::set(
        payload.as_ref(),
        &wasm_bindgen::JsValue::from_str("renderedCount"),
        &wasm_bindgen::JsValue::from_f64(rendered_count as f64),
    );
    let _ = js_sys::Reflect::set(
        window.as_ref(),
        &wasm_bindgen::JsValue::from_str("__SEQUOIA_CLAIM_LABEL_DEBUG__"),
        payload.as_ref(),
    );
}

/// Logs an uncaptured wgpu error. A broken pipeline would repeat every frame, so only the
/// first few are logged in full.
fn report_uncaptured_error(error: wgpu::Error) {
    use std::sync::atomic::{AtomicU32, Ordering};
    const LOGGED: u32 = 8;
    static SEEN: AtomicU32 = AtomicU32::new(0);
    let seen = SEEN.fetch_add(1, Ordering::Relaxed);
    if seen < LOGGED {
        web_sys::console::error_1(&format!("wgpu error: {error}").into());
    } else if seen == LOGGED {
        web_sys::console::error_1(&"wgpu error: further errors are not logged".into());
    }
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

#[expect(
    clippy::too_many_arguments,
    reason = "Low-level glyph emission takes borrowed atlas data, output and per-line geometry/style."
)]
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

#[expect(
    clippy::too_many_arguments,
    reason = "Fill and halo share line geometry but retain distinct output buffers and colors."
)]
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

// --- Tile texture cache ---

/// Identifies a tile set by id and quality, in load order.
/// Most tile pixels uploaded in one frame. wgpu keeps a staging copy of every upload until
/// the frame is submitted, so uploading a whole tile set at once (a renderer rebuilt after a
/// lost context, or a start from a warm cache) briefly needed memory for all of it.
const TILE_UPLOAD_BUDGET_BYTES: u64 = 16 << 20;

fn tile_upload_signature(tiles: &[LoadedTile]) -> u64 {
    tiles.iter().fold(0u64, |acc, tile| {
        let quality_bits = match tile.quality {
            TileQuality::Low => 1u64,
            TileQuality::High => 2u64,
        };
        acc.wrapping_mul(1_099_511_628_211)
            .wrapping_add(((tile.id as u64) << 2) ^ quality_bits)
    })
}

struct TileTexture {
    bind_group: wgpu::BindGroup,
    rect: [f32; 4], // [x, z, width, height] in world coords
    quality: TileQuality,
}

/// The cached minimap background and tiles, drawn once per layout and tile set.
struct MinimapTerrain {
    layout: MinimapLayout,
    tiles_revision: u64,
    bind_group: wgpu::BindGroup,
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
    /// Identity of the uploaded tile set; `tiles_revision` counts its changes.
    tiles_signature: Option<u64>,
    tiles_revision: u64,

    // Connection line pipeline (full GPU mode only)
    connection_pipeline: wgpu::RenderPipeline,
    connection_fill_pipeline: wgpu::RenderPipeline,
    connection_buffer: wgpu::Buffer,
    connection_count: u32,
    connection_capacity: u32,
    connection_vertices: Vec<ConnectionVertex>,
    connection_drawn_set: HashSet<(u64, u64)>,
    minimap_indicator_buffer: wgpu::Buffer,
    minimap_indicator_capacity: u32,

    // Minimap terrain cache: background and tiles rendered offscreen, composited per frame.
    minimap_terrain_viewport_buffer: wgpu::Buffer,
    minimap_terrain_viewport_bind_group: wgpu::BindGroup,
    minimap_bg_buffer: wgpu::Buffer,
    minimap_blit_pipeline: wgpu::RenderPipeline,
    minimap_blit_bind_group_layout: wgpu::BindGroupLayout,
    minimap_blit_buffer: wgpu::Buffer,
    minimap_terrain: Option<MinimapTerrain>,

    // Text pipelines (static + dynamic)
    text_renderer: Option<GpuTextRenderer>,
    /// Font the glyph atlas was built with.
    text_readable_font: bool,
    territory_name_cache: HashMap<String, (String, String)>,

    // Resource icon pipeline
    icon_renderer: Option<GpuIconRenderer>,
    supports_gpu_icons: bool,

    // Track current dimensions
    width: u32,
    height: u32,
    dpr: f32,

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
}

impl GpuRenderer {
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
        // The icon overlay pipeline uses the same textured-quad primitives as the text
        // overlay path, so it can run on the active WebGL2 renderer as well. Only fall
        // back if icon renderer initialization itself fails.
        let supports_gpu_icons = true;
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
        // wgpu's default handler panics, and a panic mid-frame leaves the whole map wedged.
        // Validation and lost-context errors are reported instead; the frame they spoil is
        // redrawn, and a lost context is rebuilt by the canvas.
        device.on_uncaptured_error(Box::new(report_uncaptured_error));

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

        let minimap_terrain_viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("minimap-terrain-viewport-ubo"),
            size: std::mem::size_of::<ViewportUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let minimap_terrain_viewport_bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("minimap-terrain-viewport-bg"),
                layout: &viewport_bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: minimap_terrain_viewport_buffer.as_entire_binding(),
                }],
            });
        let minimap_blit_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("minimap-blit-ubo"),
            size: std::mem::size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let minimap_blit_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("minimap-blit-bgl"),
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
        let minimap_blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("minimap-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("minimap_blit.wgsl").into()),
        });
        let minimap_blit_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("minimap-blit-pl"),
                bind_group_layouts: &[&minimap_blit_bind_group_layout],
                push_constant_ranges: &[],
            });
        let minimap_blit_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("minimap-blit-pipeline"),
                layout: Some(&minimap_blit_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &minimap_blit_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[vertex_layout.clone()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &minimap_blit_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // The terrain image holds premultiplied colour (see `ensure_minimap_terrain`).
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            tiles_signature: None,
            tiles_revision: 0,
            connection_pipeline,
            connection_fill_pipeline,
            connection_buffer,
            connection_count: 0,
            connection_capacity,
            connection_vertices: Vec::new(),
            connection_drawn_set: HashSet::new(),
            minimap_indicator_buffer,
            minimap_indicator_capacity,
            minimap_terrain_viewport_buffer,
            minimap_terrain_viewport_bind_group,
            minimap_bg_buffer,
            minimap_blit_pipeline,
            minimap_blit_bind_group_layout,
            minimap_blit_buffer,
            minimap_terrain: None,
            text_renderer: None,
            text_readable_font: false,
            territory_name_cache: HashMap::new(),
            icon_renderer: None,
            supports_gpu_icons,
            width,
            height,
            dpr,
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
                compatibility_fallback: !supports_gpu_icons,
            },
            frame_metrics: FrameMetrics::default(),
        };

        if !renderer.ensure_text_renderer() {
            return Err("wgpu init (webgl): failed to initialize GPU text renderer".into());
        }

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
            self.text_readable_font,
        );
        self.text_renderer.is_some()
    }

    fn ensure_icon_renderer(&mut self, icons: &ResourceAtlas) -> bool {
        if !self.supports_gpu_icons {
            return false;
        }
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
        if self.icon_renderer.is_none() {
            self.supports_gpu_icons = false;
            self.capabilities.compatibility_fallback = true;
            web_sys::console::warn_1(
                &"GPU icon overlays disabled: failed to initialize the icon renderer".into(),
            );
        }
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
        let Some(GlyphAtlas {
            bind_group_layout: text_bind_group_layout,
            fill_bind_group,
            halo_bind_group,
            glyphs,
            kerning,
            line_height,
        }) = Self::build_glyph_atlas(device, queue, readable_font)
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
        let sequoia_ornament_w = icons
            .sequoia_territory_ornament_image
            .natural_width()
            .max(1);
        let sequoia_ornament_h = icons
            .sequoia_territory_ornament_image
            .natural_height()
            .max(1);
        let icon_cell_w = (resource_atlas_w / ICON_COUNT).max(1);
        let icon_cell_h = resource_atlas_h.max(1);
        let crown_slot_w = icon_cell_w.max(crown_w);
        let crown_slot_h = crown_h;
        let resource_x = 0u32;
        let crown_x = resource_x + resource_atlas_w;
        let ornament_x = crown_x + crown_slot_w;
        let sequoia_ornament_x = ornament_x + ornament_w;
        let atlas_w = sequoia_ornament_x + sequoia_ornament_w;
        let atlas_h = icon_cell_h
            .max(crown_h)
            .max(ornament_h)
            .max(sequoia_ornament_h);
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
            crown_slot_h as f64,
        )
        .ok()?;
        ctx.draw_image_with_html_image_element(
            &icons.territory_ornament_image,
            ornament_x as f64,
            0.0,
        )
        .ok()?;
        ctx.draw_image_with_html_image_element(
            &icons.sequoia_territory_ornament_image,
            sequoia_ornament_x as f64,
            0.0,
        )
        .ok()?;
        ctx.set_image_smoothing_enabled(true);

        let image_data = ctx
            .get_image_data(0.0, 0.0, atlas_w as f64, atlas_h as f64)
            .ok()?;
        let mut pixels = image_data.data().0;

        let atlas_wf = atlas_w as f32;
        let atlas_hf = atlas_h as f32;
        let sequoia_target_gold = derive_sequoia_ornament_gold(
            &pixels,
            atlas_w,
            sequoia_ornament_x,
            sequoia_ornament_w,
            sequoia_ornament_h,
        );
        #[derive(Clone, Copy)]
        enum OrnamentColorMode {
            MonochromeMask,
            MatchSequoiaBorderGold,
        }
        let mut extract_ornament =
            |slot_x: u32, slot_w: u32, slot_h: u32, color_mode: OrnamentColorMode| {
                let mut orn_min_x = slot_w;
                let mut orn_min_y = slot_h;
                let mut orn_max_x = 0u32;
                let mut orn_max_y = 0u32;
                let mut orn_found = false;
                for y in 0..slot_h {
                    for x in 0..slot_w {
                        let atlas_px_x = slot_x + x;
                        let idx = ((y * atlas_w + atlas_px_x) * 4) as usize;
                        let r = pixels[idx];
                        let g = pixels[idx + 1];
                        let b = pixels[idx + 2];
                        let src_a = pixels[idx + 3];
                        let alpha = ornament_mask_alpha(r, g, b, src_a);
                        match color_mode {
                            OrnamentColorMode::MonochromeMask => {
                                pixels[idx] = 255;
                                pixels[idx + 1] = 255;
                                pixels[idx + 2] = 255;
                                pixels[idx + 3] = alpha;
                            }
                            OrnamentColorMode::MatchSequoiaBorderGold => {
                                if is_sequoia_ornament_neutral_highlight(r, g, b, alpha) {
                                    pixels[idx] = sequoia_target_gold[0];
                                    pixels[idx + 1] = sequoia_target_gold[1];
                                    pixels[idx + 2] = sequoia_target_gold[2];
                                }
                                pixels[idx + 3] = alpha;
                            }
                        }
                        if alpha > 6 {
                            orn_found = true;
                            orn_min_x = orn_min_x.min(x);
                            orn_min_y = orn_min_y.min(y);
                            orn_max_x = orn_max_x.max(x);
                            orn_max_y = orn_max_y.max(y);
                        }
                    }
                }
                if orn_found {
                    // Keep the ornament tight enough to hug the corner, but leave a tiny UV
                    // gutter so linear sampling does not chew into the decorative edge.
                    let padded_min_x = orn_min_x.saturating_sub(ORNAMENT_UV_PADDING_PX);
                    let padded_min_y = orn_min_y.saturating_sub(ORNAMENT_UV_PADDING_PX);
                    let padded_max_x = orn_max_x
                        .saturating_add(ORNAMENT_UV_PADDING_PX)
                        .min(slot_w.saturating_sub(1));
                    let padded_max_y = orn_max_y
                        .saturating_add(ORNAMENT_UV_PADDING_PX)
                        .min(slot_h.saturating_sub(1));
                    let tight_w = (padded_max_x - padded_min_x + 1).max(1);
                    let tight_h = (padded_max_y - padded_min_y + 1).max(1);
                    (
                        [
                            ((slot_x + padded_min_x) as f32) / atlas_wf,
                            (padded_min_y as f32) / atlas_hf,
                            ((slot_x + padded_max_x + 1) as f32) / atlas_wf,
                            ((padded_max_y + 1) as f32) / atlas_hf,
                        ],
                        (tight_w as f32 / tight_h as f32).clamp(0.2, 5.0),
                    )
                } else {
                    (
                        [
                            (slot_x as f32) / atlas_wf,
                            0.0,
                            ((slot_x + slot_w) as f32) / atlas_wf,
                            (slot_h as f32) / atlas_hf,
                        ],
                        (slot_w as f32 / slot_h as f32).clamp(0.2, 5.0),
                    )
                }
            };
        let (default_ornament_uv, default_ornament_aspect) = extract_ornament(
            ornament_x,
            ornament_w,
            ornament_h,
            OrnamentColorMode::MonochromeMask,
        );
        let (sequoia_ornament_uv, sequoia_ornament_aspect) = extract_ornament(
            sequoia_ornament_x,
            sequoia_ornament_w,
            sequoia_ornament_h,
            OrnamentColorMode::MatchSequoiaBorderGold,
        );
        let icon_v1 = (icon_cell_h as f32 / atlas_hf).clamp(0.0, 1.0);
        let mut uv_by_kind = HashMap::with_capacity((ICON_COUNT + 1) as usize);
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
                (crown_slot_h as f32) / atlas_hf,
            ],
        );

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
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
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
            default_ornament_uv,
            default_ornament_aspect,
            sequoia_ornament_uv,
            sequoia_ornament_aspect,
        })
    }

    fn build_glyph_atlas(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        readable_font: bool,
    ) -> Option<GlyphAtlas> {
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
            format!("{GLYPH_ATLAS_FONT_PX}px 'Inter', system-ui, sans-serif")
        } else {
            format!("{GLYPH_ATLAS_FONT_PX}px 'SilkscreenLocal', monospace")
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
        Some(GlyphAtlas {
            bind_group_layout,
            fill_bind_group,
            halo_bind_group,
            glyphs,
            kerning,
            line_height: line_height_px as f32,
        })
    }

    pub fn capabilities(&self) -> RenderCapabilities {
        self.capabilities
    }

    pub fn frame_metrics(&self) -> FrameMetrics {
        self.frame_metrics
    }

    /// The largest surface side the device accepts, in physical pixels.
    pub fn max_surface_side(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
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

    /// Uploads tiles that are new or improved since the last frame and drops tiles that
    /// are gone, bumping `tiles_revision` when the set changes.
    ///
    /// Uploads at most [`TILE_UPLOAD_BUDGET_BYTES`] of pixels (and always one tile) per call;
    /// returns whether tiles are still waiting, so the caller draws another frame.
    fn sync_tiles(&mut self, tiles: &[LoadedTile]) -> bool {
        let signature = tile_upload_signature(tiles);
        if self.tiles_signature == Some(signature) || !self.ensure_tile_upload_context() {
            return false;
        }
        self.tiles_signature = Some(signature);
        self.tiles_revision = self.tiles_revision.wrapping_add(1);
        let active_tile_ids: HashSet<usize> = tiles.iter().map(|tile| tile.id).collect();
        self.tile_textures
            .retain(|tile_id, _| active_tile_ids.contains(tile_id));

        let Some(upload_canvas) = self.tile_upload_canvas.as_ref().cloned() else {
            return false;
        };
        let Some(upload_ctx) = self.tile_upload_ctx.as_ref().cloned() else {
            return false;
        };
        let mut upload_size = self.tile_upload_canvas_size;
        let mut uploaded_bytes = 0u64;

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
            let bytes = 4 * u64::from(w) * u64::from(h);
            if uploaded_bytes > 0 && uploaded_bytes + bytes > TILE_UPLOAD_BUDGET_BYTES {
                // The rest go up over the next frames.
                self.tiles_signature = None;
                break;
            }
            uploaded_bytes += bytes;

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
        self.tiles_signature.is_none()
    }

    /// Build instance data from territories and upload to GPU.
    ///
    /// Animation color interpolation is handled GPU-side: we encode
    /// from_color + timing in the instance data once, and the shader
    /// computes the interpolated color every frame at zero CPU cost.
    fn update_instances(&mut self, frame: &Frame) {
        let settings = frame.settings;
        let now = frame.now_ms;
        let start_ms = self.start_time_ms;
        let start_secs = start_ms / 1000.0;

        self.instances_buf.clear();
        self.instances_buf
            .extend(frame.territories.iter().map(|(name, ct)| {
                let loc = &ct.territory.location;
                let (r, g, b) = match frame.heat {
                    Some(heat) => {
                        let take_count = heat.take_counts.get(name).copied().unwrap_or(0);
                        heat_color_for_count(take_count, heat.max_take_count)
                    }
                    None => ct.guild_color,
                };

                let is_hovered = frame.hovered == Some(name.as_str());
                let is_selected = frame.selected == Some(name.as_str());

                let resource_data = if settings.defense_highlight {
                    defense_tier_overlay_data(
                        ct.territory
                            .runtime
                            .as_ref()
                            .and_then(|runtime| runtime.defense_tier.as_deref()),
                    )
                } else if settings.resource_highlight {
                    ct.territory.resources.highlight_data()
                } else {
                    [0.0; 4]
                };
                let has_overlay =
                    resource_data[0] > 0.5 || (resource_data[3] as u32 & (1 << 10)) != 0; // mode 0 + double emeralds

                let fill_alpha = if has_overlay {
                    settings.overlay_fill_alpha()
                        + if is_selected {
                            0.18
                        } else if is_hovered {
                            0.10
                        } else {
                            0.0
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
                let is_at_war = frame.wars.is_some_and(|wars| wars.contains(name));
                let flags = (is_hovered as u32)
                    + (is_selected as u32) * 2
                    + (is_headquarters as u32) * 4
                    + (is_at_war as u32) * 8;

                let acquired_rel_secs = if settings.suppress_cooldown_visuals {
                    -1_000_000.0_f32
                } else {
                    (ct.territory.acquired.timestamp() as f64 - start_secs) as f32
                };

                // Encode animation params for GPU-side interpolation
                let (anim_color, anim_time) = if frame.heat.is_some() {
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
                        fill_alpha + settings.fill_alpha_boost,
                        0.72,
                        flags as f32,
                        if settings.suppress_cooldown_visuals {
                            1.0
                        } else if settings.thick_cooldown_borders {
                            2.0
                        } else {
                            1.0
                        },
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
        self.max_anim_end_ms = frame
            .territories
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
    fn update_static_text_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        vp: &Viewport,
        settings: &RenderSettings,
    ) {
        self.sync_territory_name_cache(territories);
        let static_tag_scale = settings.label_scales.static_tag();
        let static_name_scale = settings.label_scales.static_name();
        let Some(text_renderer) = self.text_renderer.as_mut() else {
            return;
        };

        let mut fill_instances = std::mem::take(&mut text_renderer.static_fill_instances);
        let mut halo_instances = std::mem::take(&mut text_renderer.static_halo_instances);
        fill_instances.clear();
        halo_instances.clear();

        if vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            set_claim_label_debug(vp.scale, false, 0, 0);
            text_renderer.static_fill_instances = fill_instances;
            text_renderer.static_halo_instances = halo_instances;
            text_renderer.static_fill_count = 0;
            text_renderer.static_halo_count = 0;
            return;
        }

        let scale = vp.scale as f32;
        {
            let glyphs = &text_renderer.glyphs;
            let kerning = &text_renderer.kerning;
            let line_height = text_renderer.line_height;
            let tag_tracking_units = line_height * STATIC_TAG_LETTER_SPACING_EM;
            let name_tracking_units = line_height * STATIC_NAME_LETTER_SPACING_EM;
            let claim_label_zoom = claim_label_zoom_active(vp.scale);
            if settings.show_claim_labels && claim_label_zoom {
                let claim_tracking_units = line_height * CLAIM_LABEL_LETTER_SPACING_EM;
                let claim_clusters = build_claim_clusters(territories);
                let claim_labels = select_claim_label_candidates(
                    &claim_clusters,
                    vp,
                    claim_labels::Rect {
                        left: 0.0,
                        top: 0.0,
                        right: self.surface_config.width as f32,
                        bottom: self.surface_config.height as f32,
                    },
                    line_height,
                    |text| line_units_with_tracking(text, glyphs, kerning, claim_tracking_units),
                );
                let rendered_claim_label_count = claim_labels.len();
                for claim in claim_labels {
                    let (r, g, b) = brighten(
                        claim.guild_color.0,
                        claim.guild_color.1,
                        claim.guild_color.2,
                        1.8,
                    );
                    push_text_line_dual_with_tracking(
                        &mut fill_instances,
                        &mut halo_instances,
                        glyphs,
                        kerning,
                        line_height,
                        &claim.text,
                        claim.center_world[0],
                        claim.center_world[1],
                        claim.font_height_world,
                        claim.max_width_world,
                        claim_tracking_units,
                        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.97],
                        [0.0, 0.0, 0.0, 0.84],
                    );
                }
                set_claim_label_debug(
                    vp.scale,
                    true,
                    claim_clusters.len(),
                    rendered_claim_label_count,
                );
            } else {
                set_claim_label_debug(vp.scale, false, 0, 0);
                let show_far_zoom_tags = settings.show_far_zoom_territory_tags && claim_label_zoom;
                for (name, ct) in territories {
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
                    if is_hq && hq_crown_expanded_at_zoom(scale) {
                        continue;
                    }
                    if show_far_zoom_tags {
                        if is_unclaimed_guild(
                            &ct.territory.guild.uuid,
                            &ct.territory.guild.name,
                            &ct.territory.guild.prefix,
                        ) {
                            continue;
                        }
                        let tag = ct.territory.guild.prefix.trim();
                        if tag.is_empty() {
                            continue;
                        }
                        let Some(sizing) =
                            compute_far_zoom_tag_sizing(sw, sh, scale, static_tag_scale)
                        else {
                            continue;
                        };
                        let units_per_world = line_height / sizing.font_height_world.max(0.001);
                        let fitted = fit_text_to_units_with_tracking(
                            tag,
                            sizing.max_width_world * units_per_world,
                            glyphs,
                            kerning,
                            tag_tracking_units,
                        );
                        if fitted == "..." {
                            continue;
                        }

                        let resource_icon_detail_layout_alpha =
                            compute_label_layout_metrics(sw as f64, sh as f64, false)
                                .detail_layout_alpha;
                        let resource_icons_visible = resource_icons_visible_for_territory(
                            settings.show_resource_icons,
                            sw,
                            sh,
                            resource_icon_detail_layout_alpha,
                            &ct.territory.resources,
                        );
                        let label_lift = compute_resource_icon_label_lift_world(
                            hh,
                            resource_icon_detail_layout_alpha,
                            resource_icons_visible,
                        );
                        let tag_y = loc.midpoint_y() as f32 - label_lift;
                        let mut tag_color = name_color_rgba(settings.tag_color, ct.guild_color);
                        tag_color[3] = 0.92 * sizing.alpha;
                        let tag_halo_alpha = 0.68 * sizing.alpha;
                        push_text_line_dual_with_tracking(
                            &mut fill_instances,
                            &mut halo_instances,
                            glyphs,
                            kerning,
                            line_height,
                            &fitted,
                            loc.midpoint_x() as f32,
                            tag_y,
                            sizing.font_height_world,
                            sizing.max_width_world,
                            tag_tracking_units,
                            tag_color,
                            [0.0, 0.0, 0.0, tag_halo_alpha],
                        );
                        continue;
                    }

                    let Some(sizing) = compute_static_label_sizing(ww, hh) else {
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
                    let tag_max_w = if is_hq {
                        hq_label_max_width_world(ww, tag_padding)
                    } else {
                        (ww * overflow - tag_padding).max(STATIC_TAG_MIN_WIDTH_WORLD)
                    };
                    let resource_icon_detail_layout_alpha =
                        compute_label_layout_metrics(sw as f64, sh as f64, false)
                            .detail_layout_alpha;
                    let resource_icons_visible = resource_icons_visible_for_territory(
                        settings.show_resource_icons,
                        sw,
                        sh,
                        resource_icon_detail_layout_alpha,
                        &ct.territory.resources,
                    );
                    let label_lift = compute_resource_icon_label_lift_world(
                        hh,
                        detail_layout_alpha,
                        resource_icons_visible,
                    );
                    let base_tag_y =
                        lerp_f32(cy, cy - (detail_size + 1.0) * 0.45, detail_layout_alpha)
                            - label_lift;
                    let tag_y = if is_hq {
                        hq_normal_static_tag_y(
                            loc.top() as f32,
                            ww,
                            hh,
                            cy,
                            px_per_world,
                            tag_size,
                            detail_size,
                            detail_layout_alpha,
                            label_lift,
                        )
                    } else {
                        base_tag_y
                    };
                    let tag = ct.territory.guild.prefix.as_str();
                    let tag_px = tag_size * px_per_world;
                    if tag_px >= STATIC_TAG_MIN_RENDERED_PX {
                        let tag_color = {
                            let mut c = name_color_rgba(settings.tag_color, ct.guild_color);
                            c[3] = 1.0;
                            c
                        };
                        let tag_halo_boost = 1.0 - smoothstep_f32(9.6, 13.8, tag_px);
                        let tag_halo_alpha = (0.70 - tag_halo_boost * 0.22).clamp(0.42, 0.76);

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
                    }

                    if settings.show_names && detail_layout_alpha > 0.02 {
                        let fallback_abbrev;
                        let base_name = if let Some((abbreviated, full)) =
                            self.territory_name_cache.get(name.as_str())
                        {
                            if settings.abbreviate_names {
                                abbreviated.as_str()
                            } else {
                                full.as_str()
                            }
                        } else if settings.abbreviate_names {
                            fallback_abbrev = abbreviate_name(name);
                            fallback_abbrev.as_str()
                        } else {
                            name.as_str()
                        };
                        let name_max_w = if is_hq {
                            hq_label_max_width_world(ww, 10.0)
                        } else {
                            (ww * overflow - 10.0).max(STATIC_NAME_MIN_WIDTH_WORLD)
                        };
                        let units_per_world = line_height / detail_size.max(0.001);
                        let fitted = fit_text_to_units_with_tracking(
                            base_name,
                            name_max_w * units_per_world,
                            glyphs,
                            kerning,
                            name_tracking_units,
                        );
                        let name_y = tag_y
                            + tag_size * 0.5
                            + detail_size * STATIC_NAME_BASELINE_GAP_MULTIPLIER;
                        let mut name_rgba = name_color_rgba(settings.name_color, ct.guild_color);
                        name_rgba[3] *=
                            STATIC_NAME_FILL_ALPHA_MULTIPLIER * detail_layout_alpha.clamp(0.0, 1.0);
                        let name_px = detail_size * px_per_world;
                        if name_px < STATIC_NAME_MIN_RENDERED_PX {
                            continue;
                        }
                        let name_halo_boost = 1.0 - smoothstep_f32(8.4, 12.2, name_px);
                        let name_halo_alpha = ((0.68 - name_halo_boost * 0.12)
                            * STATIC_NAME_HALO_ALPHA_MULTIPLIER
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
    }

    /// Build timer glyph instances. Returns the clock second at which the text next changes.
    fn update_dynamic_text_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        vp: &Viewport,
        settings: &RenderSettings,
        reference_time_secs: i64,
    ) -> i64 {
        let static_tag_scale = settings.label_scales.static_tag();
        let static_name_scale = settings.label_scales.static_name();
        let dynamic_label_scale = settings.label_scales.dynamic();
        let Some(text_renderer) = self.text_renderer.as_mut() else {
            return i64::MAX;
        };

        let mut fill_instances = std::mem::take(&mut text_renderer.dynamic_fill_instances);
        let mut halo_instances = std::mem::take(&mut text_renderer.dynamic_halo_instances);
        fill_instances.clear();
        halo_instances.clear();

        // Hidden timers never change on their own; zooming back in rebuilds them.
        if vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            text_renderer.dynamic_fill_instances = fill_instances;
            text_renderer.dynamic_halo_instances = halo_instances;
            text_renderer.dynamic_fill_count = 0;
            text_renderer.dynamic_halo_count = 0;
            return i64::MAX;
        }

        let scale = vp.scale as f32;
        let mut next_update_secs = i64::MAX;
        let mut text_buf = String::with_capacity(16);
        {
            let glyphs = &text_renderer.glyphs;
            let kerning = &text_renderer.kerning;
            let line_height = text_renderer.line_height;
            for ct in territories.values() {
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
                if is_hq && hq_crown_expanded_at_zoom(scale) {
                    continue;
                }
                let state =
                    dynamic_text_state(reference_time_secs, ct.territory.acquired.timestamp());
                let next_age = dynamic_label_next_update_age(
                    state.age_secs,
                    settings.show_countdown,
                    settings.granular_map_time,
                    settings.compound_map_time,
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
                let time_max_width = if is_hq {
                    hq_label_max_width_world(ww, 8.0)
                } else {
                    sizing.time_max_width
                };
                let cooldown_max_width = if is_hq {
                    hq_label_max_width_world(ww, 8.0)
                } else {
                    sizing.cooldown_max_width
                };
                let time_px = time_size * px_per_world;
                let cooldown_px = cooldown_size * px_per_world;

                let cx = loc.midpoint_x() as f32;
                let cy = loc.midpoint_y() as f32;
                let timer_visible_at_zoom = vp.scale >= TIMER_VISIBILITY_MIN_SCALE;
                let show_dynamic_cooldown = timer_visible_at_zoom
                    && settings.show_countdown
                    && state.is_fresh
                    && cooldown_px >= DYNAMIC_COOLDOWN_MIN_RENDERED_PX;
                let any_time_format = settings.show_countdown
                    || settings.granular_map_time
                    || settings.compound_map_time;
                let show_dynamic_time = timer_visible_at_zoom
                    && any_time_format
                    && !show_dynamic_cooldown
                    && time_px >= DYNAMIC_TIME_MIN_RENDERED_PX;
                let has_timer_line = show_dynamic_time || show_dynamic_cooldown;
                let resource_icons_visible = resource_icons_visible_for_territory(
                    settings.show_resource_icons,
                    sw,
                    sh,
                    detail_layout_alpha,
                    &ct.territory.resources,
                );
                let label_lift = compute_resource_icon_label_lift_world(
                    hh,
                    detail_layout_alpha,
                    resource_icons_visible,
                );
                let mut static_name_bottom = static_name_bottom_bound(
                    true,
                    settings.show_names,
                    ww,
                    hh,
                    cy,
                    px_per_world,
                    static_tag_scale,
                    static_name_scale,
                    resource_icons_visible,
                );
                if is_hq
                    && let Some(hq_label_bottom) = hq_normal_static_label_bottom_bound(
                        loc.top() as f32,
                        ww,
                        hh,
                        cy,
                        px_per_world,
                        settings.show_names,
                        static_tag_scale,
                        static_name_scale,
                        resource_icons_visible,
                    )
                {
                    static_name_bottom = Some(
                        static_name_bottom
                            .map_or(hq_label_bottom, |bottom| bottom.max(hq_label_bottom)),
                    );
                }
                let compact_bottom_y = cy + tag_size / 2.0 - label_lift;
                let mut time_y = compact_bottom_y;
                let mut content_bottom_y = compact_bottom_y;
                if has_timer_line {
                    let stacked_total_h = tag_size + detail_size + time_size + line_gap * 2.0;
                    let stacked_top_y = cy - stacked_total_h / 2.0 - label_lift;
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
                    if settings.granular_map_time {
                        write_hms(&mut text_buf, state.age_secs);
                    } else if settings.compound_map_time {
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
                            (0.68 - (1.0 - smoothstep_f32(9.5, 14.8, time_px)) * 0.12)
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
                            (0.70 - (1.0 - smoothstep_f32(10.2, 15.8, cooldown_px)) * 0.12)
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

        self.diag_dynamic_rebuilds = self.diag_dynamic_rebuilds.saturating_add(1);
        if next_update_secs == i64::MAX {
            reference_time_secs + 1
        } else {
            next_update_secs
        }
    }

    /// Build icon instances. Returns the clock second at which their layout next changes:
    /// resource icons sit below the timers, whose size and cooldown line depend on whether a
    /// territory is still fresh.
    fn update_icon_instances(
        &mut self,
        territories: &ClientTerritoryMap,
        vp: &Viewport,
        settings: &RenderSettings,
        reference_time_secs: i64,
    ) -> i64 {
        let static_tag_scale = settings.label_scales.static_tag();
        let static_name_scale = settings.label_scales.static_name();
        let dynamic_label_scale = settings.label_scales.dynamic();
        let icon_scale = settings.label_scales.icons();
        let Some(renderer) = self.icon_renderer.as_mut() else {
            return i64::MAX;
        };
        let default_ornament_uv = renderer.default_ornament_uv;
        let default_ornament_aspect = renderer.default_ornament_aspect.max(0.2);
        let sequoia_ornament_uv = renderer.sequoia_ornament_uv;
        let sequoia_ornament_aspect = renderer.sequoia_ornament_aspect.max(0.2);
        renderer.instances_buf.clear();
        if vp.scale < LABEL_VISIBILITY_MIN_SCALE {
            renderer.instance_count = 0;
            return i64::MAX;
        }
        let mut next_change_secs = i64::MAX;

        let scale = vp.scale as f32;
        for ct in territories.values() {
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
            let short_side_world = ww.min(hh).max(1.0);
            let expanded_hq_crown = is_hq && hq_crown_expanded_at_zoom(scale);
            let resource_icon_detail_layout_alpha =
                compute_label_layout_metrics(sw as f64, sh as f64, false).detail_layout_alpha;
            let resource_icons_visible = resource_icons_visible_for_territory(
                settings.show_resource_icons,
                sw,
                sh,
                resource_icon_detail_layout_alpha,
                &ct.territory.resources,
            );
            if settings.show_territory_ornaments {
                let use_sequoia_ornament =
                    is_sequoia_guild(&ct.territory.guild.name, &ct.territory.guild.prefix);
                let (base_ornament_uv, ornament_aspect, tint) = if use_sequoia_ornament {
                    (
                        sequoia_ornament_uv,
                        sequoia_ornament_aspect,
                        [1.0, 1.0, 1.0, 1.0],
                    )
                } else {
                    (
                        default_ornament_uv,
                        default_ornament_aspect,
                        compute_territory_ornament_tint(ct.guild_color),
                    )
                };
                let ornament_sizing =
                    compute_territory_ornament_sizing(ww, hh, ornament_aspect, icon_scale);
                let corner_w_world = ornament_sizing.corner_w_world;
                let corner_h_world = ornament_sizing.corner_h_world;
                if corner_w_world * px_per_world >= ORNAMENT_MIN_RENDERED_PX
                    && corner_h_world * px_per_world >= ORNAMENT_MIN_RENDERED_PX
                {
                    let inset_world = ornament_sizing.inset_world;
                    let left = loc.left() as f32 + inset_world;
                    let top = loc.top() as f32 + inset_world;
                    let right = loc.left() as f32 + ww - inset_world - corner_w_world;
                    let bottom = loc.top() as f32 + hh - inset_world - corner_h_world;
                    if right >= left + corner_w_world && bottom >= top + corner_h_world {
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
            }
            if is_hq
                && let Some(crown_uv) = renderer.uv_by_kind.get(&IconKind::HqCrown).copied()
                && let Some(static_sizing) = compute_static_label_sizing(ww, hh)
            {
                let crown_tag_size = static_sizing.tag_size * static_tag_scale;
                let min_crown_world = 1.0;
                let normal_preferred_crown_size =
                    (crown_tag_size * HQ_CROWN_SIZE_MULTIPLIER).max(min_crown_world);
                let far_crown_size_world = (short_side_world * HQ_CROWN_FAR_BOX_FRACTION)
                    .min(HQ_CROWN_FAR_MAX_RENDERED_PX / scale.max(0.0001))
                    .max(min_crown_world);
                let crown_layout = if expanded_hq_crown {
                    Some((far_crown_size_world, cy))
                } else {
                    hq_normal_crown_layout(
                        loc.top() as f32,
                        ww,
                        hh,
                        scale,
                        normal_preferred_crown_size,
                    )
                    .map(|layout| (layout.size_world, layout.center_y))
                };
                let Some((crown_size_world, crown_center_y)) = crown_layout else {
                    continue;
                };
                renderer.instances_buf.push(IconInstance {
                    rect: [
                        cx - crown_size_world * 0.5,
                        crown_center_y - crown_size_world * 0.5,
                        crown_size_world,
                        crown_size_world,
                    ],
                    uv_rect: crown_uv,
                    tint: [1.0, 1.0, 1.0, 1.0],
                });
            }

            if expanded_hq_crown {
                continue;
            }

            if !resource_icons_visible {
                continue;
            }

            let icon_kinds = resource_icon_sequence(&ct.territory.resources);
            if icon_kinds.is_empty() {
                continue;
            }

            let acquired_secs = ct.territory.acquired.timestamp();
            let state = dynamic_text_state(reference_time_secs, acquired_secs);
            if state.is_fresh {
                next_change_secs = next_change_secs.min(acquired_secs + FRESH_TERRITORY_SECS);
            }
            let detail_layout_alpha = resource_icon_detail_layout_alpha;
            let label_lift = compute_resource_icon_label_lift_world(
                hh,
                detail_layout_alpha,
                resource_icons_visible,
            );

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
            let time_px = time_size * px_per_world;
            let cooldown_px = cooldown_size * px_per_world;
            let timer_visible_at_zoom = vp.scale >= TIMER_VISIBILITY_MIN_SCALE;
            let cooldown_timer_visible = timer_visible_at_zoom
                && state.is_fresh
                && settings.show_countdown
                && cooldown_px >= DYNAMIC_COOLDOWN_MIN_RENDERED_PX;
            let any_time_format =
                settings.show_countdown || settings.granular_map_time || settings.compound_map_time;
            let show_dynamic_time = timer_visible_at_zoom
                && any_time_format
                && !cooldown_timer_visible
                && time_px >= DYNAMIC_TIME_MIN_RENDERED_PX;
            let has_timer_line = cooldown_timer_visible || show_dynamic_time;

            let mut static_name_bottom = static_name_bottom_bound(
                true,
                settings.show_names,
                ww,
                hh,
                cy,
                px_per_world,
                static_tag_scale,
                static_name_scale,
                resource_icons_visible,
            );
            if is_hq
                && let Some(hq_label_bottom) = hq_normal_static_label_bottom_bound(
                    loc.top() as f32,
                    ww,
                    hh,
                    cy,
                    px_per_world,
                    settings.show_names,
                    static_tag_scale,
                    static_name_scale,
                    resource_icons_visible,
                )
            {
                static_name_bottom = Some(
                    static_name_bottom
                        .map_or(hq_label_bottom, |bottom| bottom.max(hq_label_bottom)),
                );
            }
            let compact_bottom_y = cy + tag_size / 2.0 - label_lift;
            let mut content_bottom_y = compact_bottom_y;
            if has_timer_line {
                let stacked_total_h = tag_size + detail_size + time_size + line_gap * 2.0;
                let stacked_top_y = cy - stacked_total_h / 2.0 - label_lift;
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
            let base_icon_y = if cooldown_timer_visible {
                cooldown_anchor_y + cooldown_size / 2.0 + icon_size_world / 2.0 + icon_offset_world
            } else {
                content_bottom_y + icon_size_world / 2.0 + icon_offset_world
            };
            let Some(icon_y) = compute_resource_icon_center_y_world(
                loc.top() as f32,
                hh,
                detail_layout_alpha,
                base_icon_y,
                icon_size_world,
            ) else {
                continue;
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
        next_change_secs
    }

    fn update_connection_vertices(
        &mut self,
        territories: &ClientTerritoryMap,
        scale: f64,
        settings: &RenderSettings,
    ) {
        self.connection_vertices.clear();
        if !settings.show_connections {
            self.connection_count = 0;
            return;
        }

        let zoom_fade = smoothstep_f32(
            settings.connection_zoom_fade.0,
            settings.connection_zoom_fade.1,
            scale as f32,
        );
        if zoom_fade < 0.001 {
            self.connection_count = 0;
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
                let opacity_scale = settings.connection_opacity_scale.max(0.0);
                let thickness_scale = settings.connection_thickness_scale.max(0.2);

                let color = if settings.bold_connections {
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

                let thickness_steps = if settings.bold_connections {
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
    }

    /// Draws a frame, first rebuilding the cached layers named in `rebuild`.
    pub fn render(&mut self, frame: &Frame, rebuild: Rebuild) -> FrameOutcome {
        let vp = frame.camera;
        let now = frame.now_ms;
        let mut stats = DrawStats::default();

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
                time: self.shader_time(now),
                resolution: [w, h],
                _pad1: [self.shader_reference_time(frame.clock_secs), 0.0],
            }]),
        );
        stats.bytes_uploaded += std::mem::size_of::<ViewportUniform>() as u64;

        let tiles_pending = self.sync_tiles(frame.tiles);
        let Some(next_refresh) = self.rebuild_layers(frame, rebuild, &mut stats) else {
            // Fail closed: map rendering requires the GPU text pipeline.
            return FrameOutcome::default();
        };
        let outcome = FrameOutcome {
            animating: now < self.max_anim_end_ms || tiles_pending,
            next_refresh,
        };

        // Pre-compute glow uniforms and write buffers BEFORE the render pass
        // to avoid pipeline stalls from mid-pass buffer writes on WebGL2/glow.
        let (draw_sel_glow, draw_hov_glow) = self.write_glow_uniforms(frame, &mut stats);

        // Get surface texture
        let output = match self.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.surface_config);
                return outcome;
            }
            Err(_) => return outcome,
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
            if !frame.tiles.is_empty() {
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                for tile in frame.tiles {
                    let Some(tile_tex) = self.tile_textures.get(&tile.id) else {
                        continue;
                    };

                    let [x1, z1, tw, th] = tile_tex.rect;
                    let x2 = x1 + tw;
                    let z2 = z1 + th;

                    // World bounds culling
                    if let Some((bx1, by1, bx2, by2)) = frame.territory_bounds {
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
                    stats.tile_draw();
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
                stats.draw();
            }

            if self.connection_count > 0 {
                pass.set_pipeline(&self.connection_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.connection_buffer.slice(..));
                pass.draw(0..self.connection_count, 0..1);
                stats.draw();
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
                    stats.draw();
                }

                if draw_hov_glow {
                    pass.set_bind_group(1, &self.glow_bind_group_hov, &[]);
                    pass.draw_indexed(0..6, 0, 0..1);
                    stats.draw();
                }
            }

            if let Some(text_renderer) = self.text_renderer.as_ref() {
                pass.set_pipeline(&text_renderer.pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                for (bind_group, buffer, count) in [
                    (
                        &text_renderer.halo_bind_group,
                        &text_renderer.static_halo_buffer,
                        text_renderer.static_halo_count,
                    ),
                    (
                        &text_renderer.fill_bind_group,
                        &text_renderer.static_fill_buffer,
                        text_renderer.static_fill_count,
                    ),
                    (
                        &text_renderer.halo_bind_group,
                        &text_renderer.dynamic_halo_buffer,
                        text_renderer.dynamic_halo_count,
                    ),
                    (
                        &text_renderer.fill_bind_group,
                        &text_renderer.dynamic_fill_buffer,
                        text_renderer.dynamic_fill_count,
                    ),
                ] {
                    if count > 0 {
                        pass.set_bind_group(1, bind_group, &[]);
                        pass.set_vertex_buffer(1, buffer.slice(..));
                        pass.draw_indexed(0..6, 0, 0..count);
                        stats.draw();
                    }
                }
            }

            if let Some(icon_renderer) = self.icon_renderer.as_ref()
                && icon_renderer.instance_count > 0
            {
                pass.set_pipeline(&icon_renderer.pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_bind_group(1, &icon_renderer.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, icon_renderer.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..6, 0, 0..icon_renderer.instance_count);
                stats.draw();
            }
        }

        if let Some(layout) = frame.minimap {
            self.ensure_minimap_terrain(&mut encoder, layout, frame.tiles, &mut stats);
            self.draw_minimap(&mut encoder, &view, layout, frame, &mut stats);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        let frame_cpu_ms = (js_sys::Date::now() - now).max(0.0);
        let fps_estimate = if self.last_render_time_ms > 0.0 {
            let dt = (now - self.last_render_time_ms).max(0.0001);
            1000.0 / dt
        } else {
            0.0
        };
        self.last_render_time_ms = now;
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
            draw_calls: stats.draw_calls,
            tile_draw_calls: stats.tile_draw_calls,
            bytes_uploaded: stats.bytes_uploaded,
            resolution_scale: self.dpr,
            territory_instances: self.instance_count,
            text_instances,
            fps_estimate,
        };

        outcome
    }

    /// Seconds since init, as the shaders' animation clock.
    fn shader_time(&self, now_ms: f64) -> f32 {
        ((now_ms - self.start_time_ms) / 1000.0) as f32
    }

    /// The timer clock relative to init, which drives the cooldown borders.
    fn shader_reference_time(&self, clock_secs: i64) -> f32 {
        (clock_secs as f64 - self.start_time_ms / 1000.0) as f32
    }

    /// Rebuilds the requested cached layers, plus any whose GPU resources had to be
    /// recreated. `None` when the text pipeline cannot be created.
    fn rebuild_layers(
        &mut self,
        frame: &Frame,
        mut rebuild: Rebuild,
        stats: &mut DrawStats,
    ) -> Option<NextRefresh> {
        let settings = frame.settings;
        let vp = frame.camera;

        // A glyph atlas for another font invalidates every text layout built from it.
        if self.text_renderer.is_none() || self.text_readable_font != settings.readable_font {
            self.text_readable_font = settings.readable_font;
            self.text_renderer = None;
            if !self.ensure_text_renderer() {
                return None;
            }
            rebuild.static_labels = true;
            rebuild.dynamic_labels = true;
        }
        if self.supports_gpu_icons
            && self.icon_renderer.is_none()
            && let Some(icons) = frame.icons
            && self.ensure_icon_renderer(icons)
        {
            rebuild.icons = true;
        }

        // Animation colour interpolation is GPU-side; instances only change with the data.
        if rebuild.territories {
            self.update_instances(frame);
            stats.bytes_uploaded +=
                (self.instance_count as u64) * std::mem::size_of::<TerritoryInstance>() as u64;
        }
        if rebuild.connections {
            self.update_connection_vertices(frame.territories, vp.scale, settings);
            stats.bytes_uploaded +=
                (self.connection_count as u64) * std::mem::size_of::<ConnectionVertex>() as u64;
        }
        let text_bytes = |text: Option<&GpuTextRenderer>, dynamic: bool| {
            text.map_or(0, |text| {
                let count = if dynamic {
                    text.dynamic_fill_count + text.dynamic_halo_count
                } else {
                    text.static_fill_count + text.static_halo_count
                };
                u64::from(count) * std::mem::size_of::<TextInstance>() as u64
            })
        };
        if rebuild.static_labels {
            self.update_static_text_instances(frame.territories, vp, settings);
            stats.bytes_uploaded += text_bytes(self.text_renderer.as_ref(), false);
        }
        let mut next_refresh = NextRefresh::default();
        if rebuild.dynamic_labels {
            next_refresh.dynamic_labels = Some(self.update_dynamic_text_instances(
                frame.territories,
                vp,
                settings,
                frame.clock_secs,
            ));
            stats.bytes_uploaded += text_bytes(self.text_renderer.as_ref(), true);
        }
        let rebuilt_icons = rebuild.icons && self.icon_renderer.is_some();
        if rebuilt_icons {
            next_refresh.icons =
                Some(self.update_icon_instances(frame.territories, vp, settings, frame.clock_secs));
            if let Some(icon_renderer) = self.icon_renderer.as_ref() {
                stats.bytes_uploaded += (icon_renderer.instance_count as u64)
                    * std::mem::size_of::<IconInstance>() as u64;
            }
        }

        self.log_rebuild_diagnostics(
            vp,
            rebuild.static_labels || rebuild.dynamic_labels || rebuilt_icons,
        );
        Some(next_refresh)
    }

    fn log_rebuild_diagnostics(&mut self, vp: &Viewport, rebuilt_labels: bool) {
        let pan_only = (vp.scale - self.diag_last_vp.2).abs() < 0.000001
            && ((vp.offset_x - self.diag_last_vp.0).abs() > 0.001
                || (vp.offset_y - self.diag_last_vp.1).abs() > 0.001);
        if pan_only && !rebuilt_labels {
            self.diag_pan_only_zero_rebuild_frames =
                self.diag_pan_only_zero_rebuild_frames.saturating_add(1);
        }
        self.diag_last_vp = (vp.offset_x, vp.offset_y, vp.scale);

        if self.diag_console_logging && rebuilt_labels {
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
    }

    /// Writes the selection and hover glow uniforms; returns which glows to draw.
    fn write_glow_uniforms(&mut self, frame: &Frame, stats: &mut DrawStats) -> (bool, bool) {
        let vp = frame.camera;
        let mut draw_sel_glow = false;
        let mut draw_hov_glow = false;

        if let Some(ct) = frame.selected.and_then(|name| frame.territories.get(name)) {
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
            stats.bytes_uploaded += std::mem::size_of::<GlowUniform>() as u64;
            draw_sel_glow = true;
        }

        if let Some(ct) = frame
            .hovered
            .filter(|hovered| frame.selected != Some(*hovered))
            .and_then(|name| frame.territories.get(name))
        {
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
                    glow_color: [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.25],
                    expand: 5.0,
                    falloff: 0.035,
                    ring_width: 1.0,
                    fill_tint_alpha: 0.0,
                    fill_tint_rgb: [0.0, 0.0, 0.0],
                    _pad: 0.0,
                }]),
            );
            stats.bytes_uploaded += std::mem::size_of::<GlowUniform>() as u64;
            draw_hov_glow = true;
        }

        (draw_sel_glow, draw_hov_glow)
    }

    /// Renders the minimap background and tiles into an image the size of the minimap,
    /// unless the cached one still matches the layout and tile set.
    ///
    /// The image is drawn onto transparent black with the usual alpha blending, so it holds
    /// premultiplied colour; compositing it with premultiplied blending gives the same
    /// result as drawing the background and tiles straight onto the frame.
    fn ensure_minimap_terrain(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        layout: MinimapLayout,
        tiles: &[LoadedTile],
        stats: &mut DrawStats,
    ) {
        if self.minimap_terrain.as_ref().is_some_and(|terrain| {
            terrain.layout == layout && terrain.tiles_revision == self.tiles_revision
        }) {
            return;
        }

        let [scissor_x, scissor_y, width, height] = layout.scissor;
        let dpr = layout.device_pixel_ratio;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("minimap-terrain-tex"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // The on-screen minimap projection, with the minimap's top-left pixel as origin.
        self.queue.write_buffer(
            &self.minimap_terrain_viewport_buffer,
            0,
            bytemuck::cast_slice(&[ViewportUniform {
                offset: [
                    layout.offset[0] - scissor_x as f32 / dpr,
                    layout.offset[1] - scissor_y as f32 / dpr,
                ],
                scale: layout.scale,
                time: 0.0,
                resolution: [width as f32 / dpr, height as f32 / dpr],
                _pad1: [0.0, 0.0],
            }]),
        );
        let bg_color = [19.0 / 255.0, 22.0 / 255.0, 31.0 / 255.0, 0.88_f32];
        let (wmx, wmy, wmxx, wmxy) = layout.world;
        let pad = 200.0_f32;
        let corner = |x: f64, y: f64, pad_x: f32, pad_y: f32| ConnectionVertex {
            world_pos: [x as f32 + pad_x, y as f32 + pad_y],
            color: bg_color,
        };
        let bg_vertices = [
            corner(wmx, wmy, -pad, -pad),
            corner(wmxx, wmy, pad, -pad),
            corner(wmxx, wmxy, pad, pad),
            corner(wmx, wmy, -pad, -pad),
            corner(wmxx, wmxy, pad, pad),
            corner(wmx, wmxy, -pad, pad),
        ];
        self.queue.write_buffer(
            &self.minimap_bg_buffer,
            0,
            bytemuck::cast_slice(&bg_vertices),
        );
        stats.bytes_uploaded +=
            (std::mem::size_of::<ViewportUniform>() + std::mem::size_of_val(&bg_vertices)) as u64;

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("minimap-terrain-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.connection_fill_pipeline);
            pass.set_bind_group(0, &self.minimap_terrain_viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, self.minimap_bg_buffer.slice(..));
            pass.draw(0..6, 0..1);
            stats.draw();

            if !tiles.is_empty() {
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &self.minimap_terrain_viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                for tile in tiles {
                    let Some(tile_tex) = self.tile_textures.get(&tile.id) else {
                        continue;
                    };
                    pass.set_bind_group(1, &tile_tex.bind_group, &[]);
                    pass.draw_indexed(0..6, 0, 0..1);
                    stats.tile_draw();
                }
            }
        }

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("minimap-blit-bg"),
            layout: &self.minimap_blit_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.minimap_blit_buffer.as_entire_binding(),
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
        self.minimap_terrain = Some(MinimapTerrain {
            layout,
            tiles_revision: self.tiles_revision,
            bind_group,
        });
    }

    /// Draws the minimap: cached terrain, then the live territories, connections and the
    /// main view's outline.
    fn draw_minimap(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        layout: MinimapLayout,
        frame: &Frame,
        stats: &mut DrawStats,
    ) {
        let Some(terrain) = self.minimap_terrain.as_ref() else {
            return;
        };
        let vp = frame.camera;
        let w = self.width as f32 / self.dpr;
        let h = self.height as f32 / self.dpr;
        let [scissor_x, scissor_y, scissor_w, scissor_h] = layout.scissor;

        self.queue.write_buffer(
            &self.minimap_viewport_buffer,
            0,
            bytemuck::cast_slice(&[ViewportUniform {
                offset: layout.offset,
                scale: layout.scale,
                time: self.shader_time(frame.now_ms),
                resolution: [w, h],
                _pad1: [self.shader_reference_time(frame.clock_secs), 0.0],
            }]),
        );
        // Clip-space rectangle of the minimap for the terrain blit.
        let (surface_w, surface_h) = (self.width as f32, self.height as f32);
        let blit_rect = [
            scissor_x as f32 / surface_w * 2.0 - 1.0,
            1.0 - scissor_y as f32 / surface_h * 2.0,
            (scissor_x + scissor_w) as f32 / surface_w * 2.0 - 1.0,
            1.0 - (scissor_y + scissor_h) as f32 / surface_h * 2.0,
        ];
        self.queue.write_buffer(
            &self.minimap_blit_buffer,
            0,
            bytemuck::cast_slice(&blit_rect),
        );
        stats.bytes_uploaded +=
            (std::mem::size_of::<ViewportUniform>() + std::mem::size_of_val(&blit_rect)) as u64;

        let (tl_wx, tl_wy) = vp.screen_to_world(0.0, 0.0);
        let (br_wx, br_wy) = vp.screen_to_world(w as f64, h as f64);
        let (world_min_x, world_min_y, world_max_x, world_max_y) = layout.world;
        let world_min_x_f = world_min_x as f32;
        let world_min_y_f = world_min_y as f32;
        let world_max_x_f = world_max_x as f32;
        let world_max_y_f = world_max_y as f32;
        let left = (tl_wx.min(br_wx) as f32).clamp(world_min_x_f, world_max_x_f);
        let right = (tl_wx.max(br_wx) as f32).clamp(world_min_x_f, world_max_x_f);
        let top = (tl_wy.min(br_wy) as f32).clamp(world_min_y_f, world_max_y_f);
        let bottom = (tl_wy.max(br_wy) as f32).clamp(world_min_y_f, world_max_y_f);
        let color = [245.0 / 255.0, 197.0 / 255.0, 66.0 / 255.0, 0.95];
        let corner = |x: f32, y: f32| ConnectionVertex {
            world_pos: [x, y],
            color,
        };
        let indicator_vertices = [
            corner(left, top),
            corner(right, top),
            corner(right, top),
            corner(right, bottom),
            corner(right, bottom),
            corner(left, bottom),
            corner(left, bottom),
            corner(left, top),
        ];
        let indicator_count = if right > left && bottom > top {
            8u32
        } else {
            0u32
        };
        if indicator_count > self.minimap_indicator_capacity {
            self.minimap_indicator_capacity = indicator_count.next_power_of_two();
            self.minimap_indicator_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
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
            stats.bytes_uploaded +=
                (indicator_count as u64) * std::mem::size_of::<ConnectionVertex>() as u64;
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("minimap-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);

        pass.set_pipeline(&self.minimap_blit_pipeline);
        pass.set_bind_group(0, &terrain.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..6, 0, 0..1);
        stats.draw();

        if self.instance_count > 0 {
            pass.set_pipeline(&self.territory_pipeline);
            pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..self.instance_count);
            stats.draw();
        }

        if self.connection_count > 0 {
            pass.set_pipeline(&self.connection_pipeline);
            pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, self.connection_buffer.slice(..));
            pass.draw(0..self.connection_count, 0..1);
            stats.draw();
        }

        pass.set_pipeline(&self.connection_pipeline);
        pass.set_bind_group(0, &self.minimap_viewport_bind_group, &[]);
        pass.set_vertex_buffer(0, self.minimap_indicator_buffer.slice(..));
        pass.draw(0..indicator_count, 0..1);
        stats.draw();
    }
}

/// Draw-call and upload accounting for one frame.
#[derive(Default)]
struct DrawStats {
    draw_calls: u32,
    tile_draw_calls: u32,
    bytes_uploaded: u64,
}

impl DrawStats {
    fn draw(&mut self) {
        self.draw_calls = self.draw_calls.saturating_add(1);
    }

    fn tile_draw(&mut self) {
        self.draw();
        self.tile_draw_calls = self.tile_draw_calls.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SEQUOIA_ORNAMENT_FALLBACK_GOLD, derive_sequoia_ornament_gold, hq_normal_crown_layout,
        hq_normal_static_tag_y, is_sequoia_ornament_neutral_highlight, ornament_mask_alpha,
    };

    #[test]
    fn derives_sequoia_gold_from_existing_warm_accents() {
        let pixels = vec![
            236, 189, 74, 255, 201, 158, 57, 255, 242, 238, 232, 255, 0, 0, 0, 0,
        ];
        let derived = derive_sequoia_ornament_gold(&pixels, 4, 0, 4, 1);

        assert!((220..=236).contains(&derived[0]));
        assert!((170..=189).contains(&derived[1]));
        assert!((60..=74).contains(&derived[2]));
    }

    #[test]
    fn falls_back_to_default_gold_without_warm_samples() {
        let pixels = vec![
            242, 240, 236, 255, 230, 228, 224, 255, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            derive_sequoia_ornament_gold(&pixels, 4, 0, 4, 1),
            SEQUOIA_ORNAMENT_FALLBACK_GOLD
        );
    }

    #[test]
    fn only_neutral_bright_sequoia_pixels_get_recolored() {
        let white_alpha = ornament_mask_alpha(242, 240, 236, 255);
        let gold_alpha = ornament_mask_alpha(236, 189, 74, 255);

        assert!(is_sequoia_ornament_neutral_highlight(
            242,
            240,
            236,
            white_alpha
        ));
        assert!(!is_sequoia_ornament_neutral_highlight(
            236, 189, 74, gold_alpha
        ));
    }

    #[test]
    fn hq_normal_crown_layout_fits_top_slot() {
        let layout =
            hq_normal_crown_layout(10.0, 64.0, 160.0, 2.0, 100.0).expect("crown should fit");

        assert!((layout.size_world - 38.4).abs() < 0.001);
        assert!((layout.center_y - 31.2).abs() < 0.001);
        assert!((layout.label_clear_y - 51.9).abs() < 0.001);
    }

    #[test]
    fn hq_normal_static_tag_clears_crown_slot() {
        let tag_size = 24.0;
        let layout = hq_normal_crown_layout(10.0, 64.0, 160.0, 2.0, tag_size * 1.75)
            .expect("crown should fit");
        let tag_y = hq_normal_static_tag_y(10.0, 64.0, 160.0, 45.0, 2.0, tag_size, 21.5, 1.0, 0.0);

        assert!(tag_y - tag_size * 0.5 >= layout.label_clear_y - 0.001);
    }
}
