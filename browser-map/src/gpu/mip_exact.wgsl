// Exact mip-chain shader: draws one mip level from the level above it in integer arithmetic.
// Each texel is the rounded mean of the 2x2 block it covers, counting only texels inside the
// source: (sum + count / 2) / count, per 8-bit channel. Linear filtering rounds the same
// mean however the GPU's filtering unit does.

@group(0) @binding(0)
var source: texture_2d<f32>;

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let target_texel = vec2<i32>(in.clip_position.xy);
    let size = vec2<i32>(textureDimensions(source));
    var sum = vec4<u32>(0u);
    var count = 0u;
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let texel = target_texel * 2 + vec2<i32>(x, y);
            if (texel.x < size.x && texel.y < size.y) {
                sum += vec4<u32>(round(textureLoad(source, texel, 0) * 255.0));
                count += 1u;
            }
        }
    }
    let mean = (sum + vec4<u32>(count / 2u)) / vec4<u32>(max(count, 1u));
    return vec4<f32>(mean) / 255.0;
}
