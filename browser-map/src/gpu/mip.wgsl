// Mip-chain shader: draws one mip level from the level above it. At exactly half the size,
// linear filtering averages each 2x2 block of the source.

@group(0) @binding(0)
var source: texture_2d<f32>;

@group(0) @binding(1)
var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle that covers the target.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOutput;
    out.clip_position = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, in.uv);
}
