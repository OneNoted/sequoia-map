// Resource icon shader.

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
var icon_texture: texture_2d<f32>;

@group(1) @binding(1)
var icon_sampler: sampler;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) uv_rect: vec4<f32>,
    @location(3) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = in.rect.xy + in.quad_pos * in.rect.zw;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = mix(in.uv_rect.xy, in.uv_rect.zw, in.quad_pos);
    out.tint = in.tint;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(icon_texture, icon_sampler, in.uv);
    return vec4<f32>(tex.rgb * in.tint.rgb, tex.a * in.tint.a);
}
