// Composites the cached minimap terrain image into the minimap rectangle.
//
// The image is exactly the size of the destination rectangle in physical pixels and holds
// premultiplied colour, so a nearest-sampled quad reproduces it pixel for pixel.

struct Blit {
    // Destination rectangle in clip space: left, top, right, bottom.
    rect: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> blit: Blit;

@group(0) @binding(1)
var terrain: texture_2d<f32>;

@group(0) @binding(2)
var terrain_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) quad_pos: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    let pos = mix(blit.rect.xy, blit.rect.zw, quad_pos);
    out.clip_position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = quad_pos;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(terrain, terrain_sampler, in.uv);
}
