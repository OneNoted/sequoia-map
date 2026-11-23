// Turns a decoded tile into palette indices: each texel's premultiplied colour is looked up
// in the tile's sorted palette, and its position written as an R8 value.

@group(0) @binding(0)
var source: texture_2d<f32>;

// 256 colours, sorted by key (see `tile_palettes`).
@group(0) @binding(1)
var palette: texture_2d<f32>;

// Where the source's first texel lands in the target: x, y (zw unused). The pass draws
// only the tile's rectangle, so a tile can be written into its place in the atlas.
@group(0) @binding(2)
var<uniform> origin: vec4<i32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

// One triangle that covers the target.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOutput;
    out.clip_position = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    return out;
}

// The colour as big-endian RGBA bytes, the palette's sort key.
fn key(colour: vec4<f32>) -> u32 {
    let c = vec4<u32>(round(clamp(colour, vec4<f32>(0.0), vec4<f32>(1.0)) * 255.0));
    return (c.r << 24u) | (c.g << 16u) | (c.b << 8u) | c.a;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let wanted = key(textureLoad(source, vec2<i32>(in.clip_position.xy) - origin.xy, 0));
    // The first colour not below the wanted one; every colour of the tile is in its palette.
    var low = 0u;
    var high = 255u;
    for (var step = 0; step < 8; step++) {
        let middle = (low + high) / 2u;
        if (key(textureLoad(palette, vec2<i32>(i32(middle), 0), 0)) < wanted) {
            low = middle + 1u;
        } else {
            high = middle;
        }
    }
    return vec4<f32>(f32(low) / 255.0, 0.0, 0.0, 1.0);
}
