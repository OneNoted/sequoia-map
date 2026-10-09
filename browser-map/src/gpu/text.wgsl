// Dual-atlas text shader. Each pass samples one atlas (halo or fill).

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _time: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

@group(1) @binding(0)
var glyph_texture: texture_2d<f32>;

@group(1) @binding(1)
var glyph_sampler: sampler;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) uv_rect: vec4<f32>,
    @location(3) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = in.rect.xy + in.quad_pos * in.rect.zw;
    let glyph_origin = in.rect.xy * vp.scale + vp.offset;
    let screen = floor(glyph_origin + vec2<f32>(0.5, 0.5)) + (world_pos - in.rect.xy) * vp.scale;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = mix(in.uv_rect.xy, in.uv_rect.zw, in.quad_pos);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Derivative-aware alpha shaping to reduce grain/shimmer at distance while keeping edge contrast.
    let sampled_alpha = textureSample(glyph_texture, glyph_sampler, in.uv).a;
    let base_softness = 0.12;
    let adaptive = clamp(fwidth(sampled_alpha) * 1.25, 0.0, 0.10);
    let softness = clamp(base_softness + adaptive, 0.10, 0.22);
    let alpha = smoothstep(0.44 - softness, 0.44 + softness, sampled_alpha) * in.color.a;
    return vec4<f32>(in.color.rgb, alpha);
}
