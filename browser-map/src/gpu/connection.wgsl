struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _time: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

// The connection mesh does not depend on the camera; this applies the main camera's scale.
struct ConnectionProjection {
    // World units per main-map CSS pixel, for classic hairline spread.
    world_per_px: f32,
    // Classic zoom fade; 1 for solid lines and for non-connection vertices.
    fade: f32,
    _pad: vec2<f32>,
};

@group(1) @binding(0)
var<uniform> projection: ConnectionProjection;

struct VertexInput {
    @location(0) world_pos: vec2<f32>,
    @location(1) color: vec4<f32>,
    // CSS pixels added after projection: a solid strip's half-width, so it keeps its width
    // under any projection (main map at any zoom, minimap).
    @location(2) offset_px: vec2<f32>,
    // Main-map CSS pixels a classic hairline sits from its centreline.
    @location(3) spread_px: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn screen_to_ndc(screen: vec2<f32>) -> vec4<f32> {
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world = in.world_pos + in.spread_px * projection.world_per_px;
    out.clip_position = screen_to_ndc(world * vp.scale + vp.offset + in.offset_px);
    out.color = vec4<f32>(in.color.rgb, clamp(in.color.a * projection.fade, 0.0, 1.0));
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
