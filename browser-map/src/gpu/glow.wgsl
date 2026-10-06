// Glow shader — SDF rectangle glow for selection ring and hover effects.
// Renders as an expanded quad with analytical exponential glow falloff.

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _pad0: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

struct GlowParams {
    rect: vec4<f32>,        // left, top, width, height (world coords, expanded)
    glow_color: vec4<f32>,  // rgba
    expand: f32,            // expansion in pixels beyond territory rect
    falloff: f32,           // glow falloff (higher = tighter)
    ring_width: f32,        // ring stroke width (0 = no ring)
    fill_tint_alpha: f32,   // alpha of fill tint inside bounds
    fill_tint_rgb: vec3<f32>,
    _pad: f32,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

@group(1) @binding(0)
var<uniform> glow: GlowParams;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) size_px: vec2<f32>,
};

@vertex
fn vs_main(vertex: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    let world_pos = glow.rect.xy + vertex.quad_pos * glow.rect.zw;
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = vertex.quad_pos;
    out.size_px = glow.rect.zw * vp.scale;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Map UV from expanded quad to territory-relative coords
    let px = in.uv * in.size_px;
    let inner_min = vec2<f32>(glow.expand);
    let inner_max = in.size_px - vec2<f32>(glow.expand);
    let half_size = (inner_max - inner_min) * 0.5;
    let center = (inner_min + inner_max) * 0.5;

    // SDF: signed distance to rectangle border
    let d = abs(px - center) - half_size;
    let dist = length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);

    // Glow: exponential falloff from border
    let glow_intensity = exp(-dist * dist * glow.falloff);
    var color = vec4<f32>(glow.glow_color.rgb, glow.glow_color.a * glow_intensity);

    // Ring stroke on the border itself
    if glow.ring_width > 0.0 {
        let ring = 1.0 - smoothstep(0.0, glow.ring_width, abs(dist));
        color = mix(color, vec4<f32>(glow.glow_color.rgb, glow.glow_color.a), ring);
    }

    // Fill tint inside bounds
    if dist < 0.0 && glow.fill_tint_alpha > 0.0 {
        color = vec4<f32>(
            mix(color.rgb, glow.fill_tint_rgb, glow.fill_tint_alpha),
            max(color.a, glow.fill_tint_alpha)
        );
    }

    return color;
}
