// Host overlays (Map Intel site names, teammates): instanced quads placed in device pixels
// around world anchors, so prepared sprites land texel for pixel at any camera.

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
var atlas: texture_2d<f32>;

@group(1) @binding(1)
var atlas_sampler: sampler;

// x: device pixels per CSS pixel.
@group(1) @binding(2)
var<uniform> params: vec4<f32>;

// `OverlayKind` in overlay.rs.
const SPRITE: u32 = 0u;
const DOT: u32 = 2u;

// Teammate dot, CSS pixels: a filled circle with a centred one-pixel outline.
const DOT_RADIUS: f32 = 3.0;
const DOT_STROKE: vec4<f32> = vec4<f32>(12.0 / 255.0, 14.0 / 255.0, 23.0 / 255.0, 0.9);

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
    @location(1) world: vec2<f32>,
    // Device pixels from the rounded anchor to the quad's top-left corner.
    @location(2) offset: vec2<f32>,
    // Device pixels.
    @location(3) size: vec2<f32>,
    @location(4) uv_rect: vec4<f32>,
    @location(5) color: vec4<f32>,
    // Hidden below this camera scale.
    @location(6) min_scale: f32,
    @location(7) kind: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Device pixels from the quad's centre.
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) kind: u32,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.uv = vec2<f32>(0.0);
    out.local = vec2<f32>(0.0);
    out.color = in.color;
    out.kind = in.kind;
    if vp.scale < in.min_scale {
        // Every corner on one point outside the view: nothing is rasterized.
        out.clip_position = vec4<f32>(-2.0, -2.0, 0.0, 1.0);
        return out;
    }
    let dpr = params.x;
    let anchor = floor((in.world * vp.scale + vp.offset) * dpr + vec2<f32>(0.5));
    let device = anchor + in.offset + in.quad_pos * in.size;
    let ndc = device / (vp.resolution * dpr) * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = mix(in.uv_rect.xy, in.uv_rect.zw, in.quad_pos);
    out.local = (in.quad_pos - vec2<f32>(0.5)) * in.size;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.kind == SPRITE {
        return textureSampleLevel(atlas, atlas_sampler, in.uv, 0.0);
    }
    // Canvas arc + fill + stroke: one device pixel of antialiasing on each edge.
    let dpr = params.x;
    let aa = 1.0 / dpr;
    let d = length(in.local) / dpr - DOT_RADIUS;
    let fill = clamp(0.5 - d / aa, 0.0, 1.0);
    let stroke = clamp(0.5 - (abs(d) - 0.5) / aa, 0.0, 1.0) * DOT_STROKE.a;
    let alpha = stroke + fill * (1.0 - stroke);
    if alpha <= 0.0 {
        discard;
    }
    let rgb = (DOT_STROKE.rgb * stroke + in.color.rgb * fill * (1.0 - stroke)) / alpha;
    return vec4<f32>(rgb, alpha);
}
