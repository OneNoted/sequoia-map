// Territory rendering shader — instanced rectangles with fills, borders, bevels, and cooldown strips.
//
// Viewport uniform transforms world coordinates → clip space.
// Per-instance data provides rect position/size, color, state flags, and animation params.
// Color transition animations are computed GPU-side from encoded timing data.

struct Viewport {
    offset: vec2<f32>,
    scale: f32,
    time: f32,
    resolution: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> vp: Viewport;

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct InstanceInput {
    @location(1) rect: vec4<f32>,       // x, y, width, height (world coords)
    @location(2) color: vec4<f32>,      // r, g, b, 1.0 — target/static guild color
    @location(3) state: vec4<f32>,      // fill_alpha, border_alpha, flags, 0.0
    @location(4) cooldown: vec4<f32>,   // acquired_time_rel_secs, unused, unused, unused
    @location(5) anim_color: vec4<f32>,    // from_r, from_g, from_b, 0.0
    @location(6) anim_time: vec4<f32>,     // start_time_rel, duration_secs, 0, 0
    @location(7) resource_data: vec4<f32>,  // mode, idx_a, idx_b, flags
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) state: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) size_px: vec2<f32>,
    @location(4) acquired_rel_secs: f32,
    @location(5) anim_color: vec3<f32>,
    @location(6) anim_time: vec2<f32>,
    @location(7) resource_data: vec4<f32>,
};

fn world_to_ndc(world_pos: vec2<f32>) -> vec4<f32> {
    let screen = world_pos * vp.scale + vp.offset;
    let ndc = screen / vp.resolution * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    var out: VertexOutput;

    let world_pos = instance.rect.xy + vertex.quad_pos * instance.rect.zw;
    out.clip_position = world_to_ndc(world_pos);
    out.color = instance.color;
    out.state = instance.state;
    out.uv = vertex.quad_pos;
    out.size_px = instance.rect.zw * vp.scale;
    out.acquired_rel_secs = instance.cooldown.x;
    out.anim_color = instance.anim_color.xyz;
    out.anim_time = instance.anim_time.xy;
    out.resource_data = instance.resource_data;

    return out;
}

// --- Resource highlight helpers ---

fn resource_color_lut(idx: i32) -> vec3<f32> {
    if idx == 1 { return vec3<f32>(0.906, 0.545, 0.784); } // ore   #e78bc8
    if idx == 2 { return vec3<f32>(0.910, 0.714, 0.208); } // crops #e8b635
    if idx == 3 { return vec3<f32>(0.365, 0.561, 0.859); } // fish  #5d8fdb
    if idx == 4 { return vec3<f32>(0.361, 0.722, 0.361); } // wood  #5cb85c
    return vec3<f32>(0.651, 0.890, 0.631);                  // fallback
}

// Green checker overlay for double-emerald territories
fn emerald_checker(uv: vec2<f32>, size_px: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
    let px = uv * size_px;
    let perp = px.x + px.y;
    let period = 12.0;
    let f = fract(perp / period);
    let stripe = smoothstep(0.1, 0.15, f) * (1.0 - smoothstep(0.85, 0.9, f));
    let green = vec3<f32>(0.180, 0.545, 0.180); // #2e8b2e
    var c = mix(base, green, stripe * 0.9);
    // Dark outline at stripe edges — wider bands, near-black
    let edge_lo = smoothstep(0.04, 0.08, f) * (1.0 - smoothstep(0.14, 0.18, f));
    let edge_hi = smoothstep(0.80, 0.84, f) * (1.0 - smoothstep(0.90, 0.94, f));
    let edge = max(edge_lo, edge_hi);
    c = mix(c, vec3<f32>(0.02, 0.08, 0.02), edge);
    return c;
}

fn diagonal_coord(uv: vec2<f32>, size_px: vec2<f32>) -> f32 {
    let px = uv * size_px;
    let diag = px.x + px.y;
    let max_diag = size_px.x + size_px.y;
    return clamp(diag / max_diag, 0.0, 1.0);
}

fn hatch_overlay(uv: vec2<f32>, size_px: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
    let px = uv * size_px;
    let perp = px.x - px.y;
    let period = 10.0;
    let f = fract(perp / period);
    let stripe = smoothstep(0.15, 0.2, f) * (1.0 - smoothstep(0.8, 0.85, f));
    return mix(base, min(base + vec3<f32>(0.45), vec3<f32>(1.0)), stripe * 0.8);
}

fn count_bits_5(mask: u32) -> u32 {
    var m = mask & 31u;
    var c = 0u;
    c += m & 1u; m >>= 1u;
    c += m & 1u; m >>= 1u;
    c += m & 1u; m >>= 1u;
    c += m & 1u; m >>= 1u;
    c += m & 1u;
    return c;
}

fn nth_set_bit(mask: u32, n: u32) -> i32 {
    var m = mask & 31u;
    var found = 0u;
    for (var i = 0u; i < 5u; i++) {
        if (m & (1u << i)) != 0u {
            if found == n { return i32(i); }
            found++;
        }
    }
    return 0;
}

fn compute_resource_fill(rd: vec4<f32>, uv: vec2<f32>, size_px: vec2<f32>, guild: vec3<f32>) -> vec3<f32> {
    let mode = i32(rd.x + 0.5);
    let flags_raw = u32(rd.w + 0.5);
    let has_dbl_em = (flags_raw & 1024u) != 0u; // bit 10

    var result: vec3<f32>;

    if mode == 1 {
        // Solid single resource
        let idx = i32(rd.y + 0.5);
        result = resource_color_lut(idx);
        if (flags_raw & 1u) != 0u { result = hatch_overlay(uv, size_px, result); }
    } else if mode == 2 {
        // Diagonal split — two resources
        let idx_a = i32(rd.y + 0.5);
        let idx_b = i32(rd.z + 0.5);
        var ca = resource_color_lut(idx_a);
        var cb = resource_color_lut(idx_b);
        if (flags_raw & 1u) != 0u { ca = hatch_overlay(uv, size_px, ca); }
        if (flags_raw & 2u) != 0u { cb = hatch_overlay(uv, size_px, cb); }
        let d = diagonal_coord(uv, size_px);
        let feather = 1.5 / (size_px.x + size_px.y);
        let t = smoothstep(0.5 - feather, 0.5 + feather, d);
        result = mix(ca, cb, t);
    } else if mode == 3 {
        // Multi-stripe
        let stripe_mask = flags_raw & 31u;
        let double_mask = (flags_raw >> 5u) & 31u;
        let n = count_bits_5(stripe_mask);
        if n == 0u { return guild; }
        let d = diagonal_coord(uv, size_px);
        let band_f = d * f32(n);
        let band_i = u32(clamp(band_f, 0.0, f32(n) - 0.001));
        let res_idx = nth_set_bit(stripe_mask, band_i);
        result = resource_color_lut(res_idx);
        if (double_mask & (1u << u32(res_idx))) != 0u {
            result = hatch_overlay(uv, size_px, result);
        }
        // Anti-alias transitions between bands
        let in_band = fract(band_f);
        let aa = 1.5 / (size_px.x + size_px.y) * f32(n);
        if in_band < aa && band_i > 0u {
            let prev_idx = nth_set_bit(stripe_mask, band_i - 1u);
            var cp = resource_color_lut(prev_idx);
            if (double_mask & (1u << u32(prev_idx))) != 0u {
                cp = hatch_overlay(uv, size_px, cp);
            }
            let t = smoothstep(0.0, aa, in_band);
            result = mix(cp, result, t);
        }
    } else {
        // mode 0: guild color
        result = guild;
    }

    // Green checker overlay for double-emerald territories
    if has_dbl_em {
        result = emerald_checker(uv, size_px, result);
    }

    return result;
}

fn segment_distance_px(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let denom = max(dot(ab, ab), 0.0001);
    let t = clamp(dot(p - a, ab) / denom, 0.0, 1.0);
    let nearest = a + ab * t;
    return length(p - nearest);
}

fn corner_v_local(local_px: vec2<f32>, arm_px: f32, pad_px: f32, stroke_px: f32, aa_px: f32) -> f32 {
    let zone = pad_px + arm_px + stroke_px * 2.0;
    if local_px.x > zone || local_px.y > zone {
        return 0.0;
    }

    let apex = vec2<f32>(pad_px + arm_px * 0.72, pad_px + arm_px * 0.72);
    let edge_a = vec2<f32>(pad_px, pad_px + arm_px);
    let edge_b = vec2<f32>(pad_px + arm_px, pad_px);
    let dist_a = segment_distance_px(local_px, edge_a, apex);
    let dist_b = segment_distance_px(local_px, edge_b, apex);
    let dist = min(dist_a, dist_b);

    return 1.0 - smoothstep(stroke_px - aa_px, stroke_px + aa_px, dist);
}

fn corner_v_mask(
    uv: vec2<f32>,
    size_px: vec2<f32>,
    arm_px: f32,
    pad_px: f32,
    stroke_px: f32,
    aa_px: f32,
) -> f32 {
    let px = uv * size_px;
    let zone = pad_px + arm_px + stroke_px * 2.0;
    let near_x = px.x < zone || (size_px.x - px.x) < zone;
    let near_y = px.y < zone || (size_px.y - px.y) < zone;
    if !(near_x && near_y) {
        return 0.0;
    }

    let tl = corner_v_local(px, arm_px, pad_px, stroke_px, aa_px);
    let tr = corner_v_local(vec2<f32>(size_px.x - px.x, px.y), arm_px, pad_px, stroke_px, aa_px);
    let bl = corner_v_local(vec2<f32>(px.x, size_px.y - px.y), arm_px, pad_px, stroke_px, aa_px);
    let br = corner_v_local(
        vec2<f32>(size_px.x - px.x, size_px.y - px.y),
        arm_px,
        pad_px,
        stroke_px,
        aa_px,
    );
    return max(max(tl, tr), max(bl, br));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let fill_alpha = in.state.x;
    let zoom_fill_boost = smoothstep(0.40, 0.12, vp.scale) * 0.24;
    let border_alpha = in.state.y;
    let flags = in.state.z;
    let reference_rel_secs = vp._pad1.x;
    let age_secs = max(reference_rel_secs - in.acquired_rel_secs, 0.0);
    var cooldown_frac: f32 = 0.0;
    if age_secs < 600.0 {
        cooldown_frac = clamp((600.0 - age_secs) / 600.0, 0.0, 1.0);
    }

    let is_hovered = (u32(flags) & 1u) != 0u;
    let is_selected = (u32(flags) & 2u) != 0u;
    let is_headquarters = (u32(flags) & 4u) != 0u;

    // GPU-side color animation
    var base_color = in.color.rgb;
    var flash: f32 = 0.0;
    let anim_dur = in.anim_time.y;
    if anim_dur > 0.0 {
        let elapsed = vp.time - in.anim_time.x;
        if elapsed < anim_dur {
            let raw_t = clamp(elapsed / anim_dur, 0.0, 1.0);
            let t1 = raw_t - 1.0;
            let eased = t1 * t1 * t1 + 1.0; // cubic ease-out
            base_color = mix(in.anim_color, in.color.rgb, eased);
        }
        // Flash: 0.6→0 over first 200ms
        if elapsed >= 0.0 && elapsed < 0.2 {
            flash = (1.0 - elapsed / 0.2) * 0.6;
        }
    }

    // Distance from edge in pixels
    let dx = min(in.uv.x, 1.0 - in.uv.x) * in.size_px.x;
    let dy = min(in.uv.y, 1.0 - in.uv.y) * in.size_px.y;
    let edge_dist = min(dx, dy);

    // All sizes in world blocks × vp.scale → CSS pixels
    // Proportions stay constant at every zoom level — no fwidth floor
    var border_mult = 1.0;
    if cooldown_frac > 0.0 {
        border_mult = in.state.w;
    }
    let border_width = max(3.2 * border_mult * vp.scale, 0.8);
    let feather = max(0.4 * vp.scale, 0.15);

    // Outer edge AA: fade alpha at territory boundary
    let outer_x = smoothstep(0.0, feather, dx);
    let outer_y = smoothstep(0.0, feather, dy);
    let outer_aa = outer_x * outer_y;

    // Inner edge: border→fill transition
    let border_tx = smoothstep(border_width + feather, border_width - feather, dx);
    let border_ty = smoothstep(border_width + feather, border_width - feather, dy);
    let border_t = max(border_tx, border_ty);

    // Border alpha (state-dependent)
    var b_alpha: f32;
    if is_selected {
        b_alpha = 0.85;
    } else if is_hovered {
        b_alpha = 0.75;
    } else {
        b_alpha = border_alpha;
    }
    if is_headquarters {
        b_alpha = max(b_alpha, 0.88);
    }

    // Fill zone — compute color and alpha with all effects
    var fill_color = compute_resource_fill(in.resource_data, in.uv, in.size_px, base_color);
    if is_headquarters {
        let hq_tint = vec3<f32>(0.973, 0.831, 0.275);
        fill_color = mix(fill_color, hq_tint, 0.06);
    }
    var f_alpha = fill_alpha + zoom_fill_boost;
    var cooldown_strip_mix: f32 = 0.0;
    var cooldown_strip_color: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);

    // Flash overlay (warm gold)
    if flash > 0.0 {
        let gold = vec3<f32>(1.0, 0.851, 0.4);
        fill_color = mix(fill_color, gold, flash * 0.6);
        f_alpha = max(f_alpha, flash * 0.4);
    }

    // Bevel zone (MC-style inner highlight/shadow)
    let bevel_width = 1.0 * vp.scale;  // 1 block in world-space
    let show_bevel = in.size_px.x > 40.0 && in.size_px.y > 40.0;
    if show_bevel {
        let bevel_dist = edge_dist - border_width;
        if bevel_dist > 0.0 && bevel_dist < bevel_width {
            let px_from_left = in.uv.x * in.size_px.x;
            let px_from_top = in.uv.y * in.size_px.y;
            let px_from_right = (1.0 - in.uv.x) * in.size_px.x;
            let px_from_bottom = (1.0 - in.uv.y) * in.size_px.y;

            let is_top = px_from_top - border_width < bevel_width;
            let is_left = px_from_left - border_width < bevel_width;
            let is_bottom = px_from_bottom - border_width < bevel_width;
            let is_right = px_from_right - border_width < bevel_width;

            if is_top || is_left {
                // Top-left highlight
                fill_color = mix(fill_color, vec3<f32>(1.0), 0.06 / max(f_alpha, 0.01));
            } else if is_bottom || is_right {
                // Bottom-right shadow
                fill_color = mix(fill_color, vec3<f32>(0.0), 0.15 / max(f_alpha, 0.01));
            }
        }
    }

    // Cooldown urgency system — strip + fill wash + pulse
    if cooldown_frac > 0.0 {
        let urgency = 1.0 - cooldown_frac;  // 0 at fresh → 1 at expiry

        // 4-step color: green → yellow → orange → red at 2.5m intervals
        var cd_color: vec3<f32>;
        if urgency < 0.25 {
            cd_color = vec3<f32>(0.400, 0.800, 0.400);  // green  (0–2.5m)
        } else if urgency < 0.50 {
            cd_color = vec3<f32>(0.961, 0.773, 0.259);  // yellow (2.5–5m)
        } else if urgency < 0.75 {
            cd_color = vec3<f32>(0.961, 0.620, 0.259);  // orange (5–7.5m)
        } else {
            cd_color = vec3<f32>(0.922, 0.341, 0.341);  // red    (7.5–10m)
        }

        // Smooth pulse ramp: 0 at urgency 0.5, 1 at urgency 1.0
        let pulse_t = saturate((urgency - 0.5) * 2.0);
        let pulse_intensity = pulse_t * pulse_t * (3.0 - 2.0 * pulse_t);  // smoothstep
        let frequency = mix(1.0, 4.0, pulse_intensity);  // 1→4 Hz
        let pulse = (sin(vp.time * frequency * 6.2832) * 0.5 + 0.5) * pulse_intensity;

        // Strip: grows from left, brightens with urgency, pulses when urgent
        let strip_h = clamp(in.size_px.y * 0.11, 4.0, 8.0);
        let px_from_bottom = (1.0 - in.uv.y) * in.size_px.y;
        if px_from_bottom < strip_h && in.uv.x < (1.0 - cooldown_frac) {
            let strip_alpha = 0.55 + urgency * 0.35 + pulse * 0.10;
            fill_color = mix(fill_color, cd_color, strip_alpha);
            f_alpha = max(f_alpha, strip_alpha);
            cooldown_strip_mix = max(cooldown_strip_mix, 0.75 + urgency * 0.2 + pulse * 0.05);
            cooldown_strip_color = cd_color;
        }

        // Fill wash: smooth ramp from urgency 0.5→1.0
        let fill_t = saturate((urgency - 0.5) * 2.0);
        let fill_smooth = fill_t * fill_t * (3.0 - 2.0 * fill_t);  // smoothstep
        let wash_alpha = fill_smooth * (0.04 + pulse * 0.06);
        if wash_alpha > 0.001 {
            fill_color = mix(fill_color, cd_color, wash_alpha);
            f_alpha = max(f_alpha, f_alpha + wash_alpha * 0.5);
        }
    }

    // Blend border and fill with anti-aliased transition
    var border_color = base_color;
    if is_headquarters {
        border_color = mix(base_color, vec3<f32>(0.973, 0.831, 0.275), 0.8);
    }
    var color = mix(fill_color, border_color, border_t);
    var alpha = mix(f_alpha, b_alpha, border_t) * outer_aa;
    if cooldown_strip_mix > 0.0 {
        color = mix(color, cooldown_strip_color, cooldown_strip_mix);
        alpha = max(alpha, 0.9);
    }

    // Corner ornament "V" accents for territory definition.
    let min_side = min(in.size_px.x, in.size_px.y);
    let corner_visibility = smoothstep(26.0, 56.0, min_side);
    if corner_visibility > 0.0 {
        let corner_pad = clamp(border_width + 1.0, 1.2, 4.8);
        let corner_arm = clamp(min_side * 0.20, 6.0, 20.0);
        let corner_stroke = clamp(0.95 * vp.scale, 0.8, 1.6);
        let corner_aa = max(feather * 0.9, 0.22);
        let corner_mask = corner_v_mask(
            in.uv,
            in.size_px,
            corner_arm,
            corner_pad,
            corner_stroke,
            corner_aa,
        );
        if corner_mask > 0.0 {
            var accent_color = mix(base_color, vec3<f32>(1.0), 0.42);
            if is_headquarters {
                accent_color = mix(accent_color, vec3<f32>(0.973, 0.831, 0.275), 0.55);
            }
            let shadow_strength = corner_mask * corner_visibility * 0.12;
            let accent_strength = corner_mask * corner_visibility * (0.18 + b_alpha * 0.24);
            color = mix(color, base_color * 0.36, shadow_strength);
            color = mix(color, accent_color, accent_strength);
            alpha = max(alpha, min(1.0, alpha + accent_strength * 0.24));
        }
    }

    return vec4<f32>(color, alpha);
}
