struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _time: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

struct VertexInput {
    @location(0) world_pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn world_to_ndc(world_pos: vec2<f32>) -> vec4<f32> {
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = world_to_ndc(in.world_pos);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
