// The terrain atlas (see `terrain_layout`): the main tiles' palette indices laid out like the
// world, drawn as one quad. Each texel takes its palette from the tile of its cell; a cell
// whose tile has not loaded draws nothing. Texels and mip levels are picked as `tile.wgsl`
// picks them from a tile's own full RGBA chain.

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

struct Atlas {
    // World x and z of the first texel, and the size in texels (one block each).
    origin: vec2<f32>,
    size: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

@group(1) @binding(0)
var<uniform> atlas: Atlas;

// Palette indices at full resolution, as R8 values.
@group(1) @binding(1)
var index_texture: texture_2d<f32>;

// The atlas's levels 2 and coarser, in RGBA.
@group(1) @binding(2)
var coarse: texture_2d<f32>;

@group(1) @binding(3)
var coarse_sampler: sampler;

// One row of 256 premultiplied colours per tile.
@group(1) @binding(4)
var palettes: texture_2d<f32>;

// Per cell of 512x512 texels: its tile's number plus one, or 0 while that has not loaded.
@group(1) @binding(5)
var cells: texture_2d<f32>;

const CELL: i32 = 512;

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
    let world_pos = atlas.origin + vertex.quad_pos * atlas.size;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y * vp.y_flip, 0.0, 1.0);
    out.uv = vertex.quad_pos;
    return out;
}

fn colour_at(texel: vec2<i32>, row: i32) -> vec4<f32> {
    let index = i32(round(textureLoad(index_texture, texel, 0).r * 255.0));
    return textureLoad(palettes, vec2<i32>(index, row), 0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // One level sharper than the footprint, as `tile.wgsl` samples.
    let ddx = dpdx(in.uv) * 0.5;
    let ddy = dpdy(in.uv) * 0.5;
    let size = atlas.size;
    let footprint = max(length(ddx * size), length(ddy * size));
    let last = vec2<i32>(size) - vec2<i32>(1);
    let texel = clamp(vec2<i32>(floor(in.uv * size)), vec2<i32>(0), last);
    let tile = i32(round(textureLoad(cells, texel / CELL, 0).r * 255.0));
    if (tile == 0) {
        return vec4<f32>(0.0);
    }
    let row = tile - 1;
    // Nearest-mipmap sampling takes level 0 up to a footprint of 2^(1/2) texels and level 1
    // up to 2^(3/2); clamp-to-edge nearest texels. Tiles start on even texels, so a level-1
    // block never spans two tiles.
    if (footprint <= 1.4142135) {
        return colour_at(texel, row);
    }
    if (footprint <= 2.828427) {
        let half_last = vec2<i32>(size * 0.5) - vec2<i32>(1);
        let base = clamp(vec2<i32>(floor(in.uv * size * 0.5)), vec2<i32>(0), half_last) * 2;
        let sum = colour_at(base, row) + colour_at(base + vec2<i32>(1, 0), row)
            + colour_at(base + vec2<i32>(0, 1), row) + colour_at(base + vec2<i32>(1, 1), row);
        // Stored as an 8-bit level would be.
        return floor(sum * 0.25 * 255.0 + 0.5) / 255.0;
    }
    return textureSampleGrad(coarse, coarse_sampler, in.uv, ddx, ddy);
}
