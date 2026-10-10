// Map Intel markers: instanced quads of a constant CSS-pixel size around world positions,
// shaped per fragment from a signed distance in CSS pixels (negative inside).

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    _time: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

// Sizes from `map_markers::marker_style`, CSS pixels.
struct MarkerStyle {
    // Node radius, plain-square node side (0 where nodes show their shape), summary side,
    // one device pixel.
    node: vec4<f32>,
    // Event, raid and camp radius, site outline width.
    sites: vec4<f32>,
    // Site outline colour.
    stroke: vec4<f32>,
};

@group(1) @binding(0)
var<uniform> style: MarkerStyle;

// `MarkerShape::code`.
const SUMMARY: u32 = 0u;
const DOT: u32 = 1u;
const CORNER: u32 = 2u;
const WALL: u32 = 3u;
const EVENT: u32 = 4u;
const RAID: u32 = 5u;
const CAMP: u32 = 6u;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
    @location(1) world: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) shape: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) shape: u32,
};

// Distance from the centre to the farthest painted pixel, before antialiasing.
fn extent(shape: u32) -> f32 {
    if shape == SUMMARY {
        return style.node.z * 0.5;
    }
    if shape <= WALL {
        if style.node.y > 0.0 {
            return style.node.y * 0.5;
        }
        return style.node.x;
    }
    // Mitred outline corners reach past the vertices, most at the camp's apex.
    let outline = style.sites.w * 2.0;
    if shape == EVENT {
        return style.sites.x + outline;
    }
    if shape == RAID {
        return style.sites.y + outline;
    }
    return style.sites.z + outline;
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // One device pixel around the shape holds its antialiased edge.
    let half = extent(in.shape) + style.node.w;
    let local = (in.quad_pos * 2.0 - 1.0) * half;
    let screen = in.world * vp.scale + vp.offset + local;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    out.clip_position = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = local;
    out.color = in.color;
    out.shape = in.shape;
    return out;
}

fn box_distance(p: vec2<f32>, half: vec2<f32>) -> f32 {
    let d = abs(p) - half;
    return max(d.x, d.y);
}

fn shape_distance(p: vec2<f32>, shape: u32) -> f32 {
    if shape == SUMMARY {
        return box_distance(p, vec2<f32>(style.node.z * 0.5));
    }
    if shape <= WALL && style.node.y > 0.0 {
        return box_distance(p, vec2<f32>(style.node.y * 0.5));
    }
    let r = style.node.x;
    if shape == DOT {
        return length(p) - r;
    }
    if shape == CORNER {
        return box_distance(p, vec2<f32>(r));
    }
    if shape == WALL {
        return min(box_distance(p, vec2<f32>(r, r * 0.45)), box_distance(p, vec2<f32>(r * 0.45, r)));
    }
    if shape == EVENT {
        return (abs(p.x) + abs(p.y) - style.sites.x) * 0.70710678;
    }
    if shape == RAID {
        return box_distance(p, vec2<f32>(style.sites.y));
    }
    // Camp: apex (0, -r), base corners (+-r, 0.85 r); y points down the screen.
    let c = style.sites.z;
    let side = normalize(vec2<f32>(1.85, -1.0));
    let apex = vec2<f32>(0.0, -c);
    let right = dot(side, p - apex);
    let left = dot(vec2<f32>(-side.x, side.y), p - apex);
    return max(max(right, left), p.y - 0.85 * c);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // One device pixel in CSS pixels: edges get the same one-pixel ramp as canvas fills.
    let aa = max(fwidth(in.local.x), 0.0001);
    let d = shape_distance(in.local, in.shape);
    let fill = clamp(0.5 - d / aa, 0.0, 1.0);
    if in.shape < EVENT {
        return vec4<f32>(in.color.rgb, in.color.a * fill);
    }
    // Sites: a centred outline over the fill.
    let outline = clamp(0.5 - (abs(d) - style.sites.w * 0.5) / aa, 0.0, 1.0) * style.stroke.a;
    let alpha = outline + fill * (1.0 - outline);
    if alpha <= 0.0 {
        discard;
    }
    let rgb = (style.stroke.rgb * outline + in.color.rgb * fill * (1.0 - outline)) / alpha;
    return vec4<f32>(rgb, alpha);
}
