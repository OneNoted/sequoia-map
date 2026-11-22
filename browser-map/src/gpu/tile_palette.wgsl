// Tile shader for tiles kept as palette indices: the same texels `tile.wgsl` samples from a
// full RGBA mip chain, from a full-resolution index image, the tile's palette and RGBA levels
// from 2 down. Level 1 is averaged from the index image as the mip chain makes it.

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    // Padding up to `y_flip`: the uniform's other fields, read only by other shaders.
    _cooldown: vec2<f32>,
    _interaction: vec4<i32>,
    _territory: vec2<f32>,
    _dpr: f32,
    // -1 when drawing into the canvas, 1 into a texture (see `ViewportUniform::y_flip`).
    y_flip: f32,
};

struct TileRect {
    rect: vec4<f32>, // left, top, width, height (world coords)
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

@group(1) @binding(0)
var<uniform> tile_rect: TileRect;

// Palette indices at full resolution, as R8 values.
@group(1) @binding(1)
var tile_index: texture_2d<f32>;

@group(1) @binding(2)
var tile_sampler: sampler;

// 256 premultiplied colours.
@group(1) @binding(3)
var tile_palette: texture_2d<f32>;

// The full chain's levels 2 and coarser.
@group(1) @binding(4)
var tile_mips: texture_2d<f32>;

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
    // As in `tile.wgsl`: a tiny overlap against seams, texels kept on their blocks.
    let pad_world = min(0.49, 0.5 / max(vp.scale, 0.001));
    let pad_sign = vertex.quad_pos * 2.0 - vec2<f32>(1.0, 1.0);
    let pad = pad_sign * pad_world;
    let world_pos = tile_rect.rect.xy + vertex.quad_pos * tile_rect.rect.zw + pad;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y * vp.y_flip, 0.0, 1.0);
    out.uv = vertex.quad_pos + pad / tile_rect.rect.zw;
    return out;
}

fn colour_at(texel: vec2<i32>) -> vec4<f32> {
    let index = i32(round(textureLoad(tile_index, texel, 0).r * 255.0));
    return textureLoad(tile_palette, vec2<i32>(index, 0), 0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // One level sharper than the footprint, as `tile.wgsl` samples.
    let ddx = dpdx(in.uv) * 0.5;
    let ddy = dpdy(in.uv) * 0.5;
    let size = vec2<f32>(textureDimensions(tile_index));
    let footprint = max(length(ddx * size), length(ddy * size));
    // Nearest-mipmap sampling takes level 0 up to a footprint of 2^(1/2) texels and level 1
    // up to 2^(3/2); clamp-to-edge nearest texels.
    if (footprint <= 1.4142135) {
        let last = vec2<i32>(size) - vec2<i32>(1);
        return colour_at(clamp(vec2<i32>(floor(in.uv * size)), vec2<i32>(0), last));
    }
    if (footprint <= 2.828427) {
        let half_size = size * 0.5;
        let last = vec2<i32>(half_size) - vec2<i32>(1);
        let base = clamp(vec2<i32>(floor(in.uv * half_size)), vec2<i32>(0), last) * 2;
        let sum = colour_at(base) + colour_at(base + vec2<i32>(1, 0))
            + colour_at(base + vec2<i32>(0, 1)) + colour_at(base + vec2<i32>(1, 1));
        // Stored as an 8-bit level would be.
        return floor(sum * 0.25 * 255.0 + 0.5) / 255.0;
    }
    return textureSampleGrad(tile_mips, tile_sampler, in.uv, ddx, ddy);
}
