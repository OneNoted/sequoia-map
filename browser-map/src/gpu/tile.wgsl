// Tile shader — textured quad.

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

struct TileRect {
    rect: vec4<f32>, // left, top, width, height (world coords)
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

@group(1) @binding(0)
var<uniform> tile_rect: TileRect;

@group(1) @binding(1)
var tile_texture: texture_2d<f32>;

@group(1) @binding(2)
var tile_sampler: sampler;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Expand each tile quad by a tiny amount to avoid sub-pixel seams
    // between adjacent tiles on some WebGL compositors (notably Gecko).
    let pad_world = min(0.49, 0.5 / max(vp.scale, 0.001));
    let pad_sign = vertex.quad_pos * 2.0 - vec2<f32>(1.0, 1.0);
    let world_pos = tile_rect.rect.xy + vertex.quad_pos * tile_rect.rect.zw + pad_sign * pad_world;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = vertex.quad_pos;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(tile_texture, tile_sampler, in.uv);
}
