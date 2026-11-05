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
    // between adjacent tiles on some WebGL compositors (notably Gecko). The texture
    // coordinates extend with it, so texels stay on their blocks; the clamped edge texel
    // fills the overlap.
    let pad_world = min(0.49, 0.5 / max(vp.scale, 0.001));
    let pad_sign = vertex.quad_pos * 2.0 - vec2<f32>(1.0, 1.0);
    let pad = pad_sign * pad_world;
    let world_pos = tile_rect.rect.xy + vertex.quad_pos * tile_rect.rect.zw + pad;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = vertex.quad_pos + pad / tile_rect.rect.zw;

    return out;
}

// Terrain stays sharp and steady as it moves. Where a texel spans more than a pixel, the
// sample point holds the texel's middle and crosses into the next texel only within half a
// pixel of their edge: square texels with a one-pixel blend between them, so an edge slides
// with a sub-pixel pan instead of jumping a whole pixel. Where many texels share a pixel, the
// trilinear sampler reads their average from the mip chain instead of whichever one the
// pixel centre lands on.
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(tile_texture, 0));
    let texel = in.uv * size;
    let texels_per_pixel = max(fwidth(texel), vec2<f32>(1e-6));
    let edge = floor(texel + 0.5);
    let blended = edge + clamp((texel - edge) / texels_per_pixel, vec2<f32>(-0.5), vec2<f32>(0.5));
    let uv = select(in.uv, blended / size, texels_per_pixel < vec2<f32>(1.0));
    // A mip level sharper than the footprint alone picks: minified terrain keeps its
    // detail, at a little of the averaging that keeps it steady.
    let sharpen = 0.5;
    return textureSampleGrad(tile_texture, tile_sampler, uv, dpdx(in.uv) * sharpen, dpdy(in.uv) * sharpen);
}
