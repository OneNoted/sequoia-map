#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) const STATIC_NAME_BASELINE_GAP_MULTIPLIER: f32 = 1.0;

const DYNAMIC_TIME_SIZE_MULTIPLIER: f32 = 0.18;
const DYNAMIC_TIME_STALE_SCALE: f32 = 0.96;
const DYNAMIC_COOLDOWN_SIZE_MULTIPLIER: f32 = 0.305;
const DYNAMIC_COOLDOWN_LARGE_SOFTEN_START_WORLD: f32 = 86.0;
const DYNAMIC_COOLDOWN_LARGE_SOFTEN_END_WORLD: f32 = 188.0;
const DYNAMIC_COOLDOWN_LARGE_SOFTEN_MIN: f32 = 0.78;
const DYNAMIC_TIMER_SMALL_TILE_IN_PX: f32 = 44.0;
const DYNAMIC_TIMER_SMALL_TILE_OUT_PX: f32 = 96.0;
const DYNAMIC_TIME_MIN_PX: f32 = 13.0;
const DYNAMIC_COOLDOWN_MIN_PX: f32 = 14.5;
const DYNAMIC_TIME_MAX_WIDTH_BONUS: f32 = 0.76;
const DYNAMIC_COOLDOWN_MAX_WIDTH_BONUS: f32 = 1.02;
const DYNAMIC_COOLDOWN_MIN_WORLD: f32 = 11.2;
const DYNAMIC_COOLDOWN_MAX_WORLD: f32 = 66.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StaticLabelSizing {
    pub detail_layout_alpha: f32,
    pub tag_size: f32,
    pub detail_size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DynamicLabelSizing {
    pub small_timer_factor: f32,
    pub tag_size: f32,
    pub detail_size: f32,
    pub time_size: f32,
    pub cooldown_size: f32,
    pub line_gap: f32,
    pub time_max_width: f32,
    pub cooldown_max_width: f32,
}

#[inline]
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[inline]
fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn small_territory_timer_factor(sw: f32, sh: f32) -> f32 {
    let tight_axis = sw.min(sh);
    1.0 - smoothstep_f32(
        DYNAMIC_TIMER_SMALL_TILE_IN_PX,
        DYNAMIC_TIMER_SMALL_TILE_OUT_PX,
        tight_axis,
    )
}

#[inline]
fn dynamic_cooldown_size_soften(box_size: f32) -> f32 {
    let t = smoothstep_f32(
        DYNAMIC_COOLDOWN_LARGE_SOFTEN_START_WORLD,
        DYNAMIC_COOLDOWN_LARGE_SOFTEN_END_WORLD,
        box_size,
    );
    lerp_f32(1.0, DYNAMIC_COOLDOWN_LARGE_SOFTEN_MIN, t)
}

#[inline]
fn static_tag_size_base(ww: f32, hh: f32, scale: f32) -> Option<f32> {
    if ww < 8.0 || hh < 6.0 {
        return None;
    }
    let px_per_world = scale.max(0.0001);
    let min_tag_world = (15.0 / px_per_world).min(ww * 0.55);
    let tag_floor = 7.2_f32.max(min_tag_world);
    let tag_cap = 28.0_f32.max(tag_floor * 1.08);
    Some((ww * 0.44).clamp(tag_floor, tag_cap))
}

#[inline]
fn dynamic_tag_size_base(ww: f32, hh: f32, scale: f32, sw: f32, sh: f32) -> Option<f32> {
    if sw < 10.0 || sh < 8.0 {
        return None;
    }
    if sw < 28.0 || sh < 18.0 {
        return None;
    }

    let box_size = ww.min(hh);
    let px_per_world = scale.max(0.0001);
    let min_tag_world = (9.6 / px_per_world).min(box_size * 0.40);
    let tag_floor = 6.0_f32.max(min_tag_world);
    let tag_cap = 76.0_f32.max(tag_floor * 1.08);
    Some((box_size * 0.236).clamp(tag_floor, tag_cap))
}

#[inline]
fn timer_max_width_world(ww: f32, small_factor: f32, width_bonus: f32) -> f32 {
    let base = (ww - 8.0).max(3.0);
    base + ww.max(1.0) * width_bonus * small_factor.clamp(0.0, 1.0)
}

pub(crate) fn compute_static_label_sizing(
    ww: f32,
    hh: f32,
    scale: f32,
) -> Option<StaticLabelSizing> {
    let tag_size = static_tag_size_base(ww, hh, scale)?;
    let detail_layout_x = smoothstep_f32(14.0, 36.0, ww);
    let detail_layout_y = smoothstep_f32(9.0, 24.0, hh);
    let detail_layout_alpha = (detail_layout_x * detail_layout_y).sqrt();

    let px_per_world = scale.max(0.0001);
    let min_name_world = (13.5 / px_per_world).min(ww * 0.40);
    let detail_floor = 5.6_f32.max(min_name_world);
    let detail_cap = 16.0_f32.max(detail_floor * 1.08);
    let detail_size = (tag_size * 0.56).clamp(detail_floor, detail_cap);

    Some(StaticLabelSizing {
        detail_layout_alpha,
        tag_size,
        detail_size,
    })
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn static_name_bottom_bound(
    use_static_gpu_labels: bool,
    static_show_names: bool,
    ww: f32,
    hh: f32,
    cy: f32,
    scale: f32,
    tag_scale: f32,
    name_scale: f32,
) -> Option<f32> {
    if !use_static_gpu_labels || !static_show_names {
        return None;
    }

    let sizing = compute_static_label_sizing(ww, hh, scale)?;
    let detail_layout_alpha = sizing.detail_layout_alpha;
    if detail_layout_alpha <= 0.02 {
        return None;
    }
    let tag_size = sizing.tag_size * tag_scale.clamp(0.5, 4.0);
    let detail_size = sizing.detail_size * name_scale.clamp(0.5, 4.0);

    let tag_y = lerp_f32(cy, cy - (detail_size + 1.0) * 0.45, detail_layout_alpha);
    let name_y = tag_y + tag_size * 0.5 + detail_size * STATIC_NAME_BASELINE_GAP_MULTIPLIER;
    Some(name_y + detail_size * 0.5)
}

pub(crate) fn compute_dynamic_label_sizing(
    ww: f32,
    hh: f32,
    scale: f32,
    dynamic_label_scale: f32,
    is_fresh: bool,
) -> Option<DynamicLabelSizing> {
    let sw = ww * scale;
    let sh = hh * scale;
    let tag_size_base = dynamic_tag_size_base(ww, hh, scale, sw, sh)?;

    let box_size = ww.min(hh);
    let px_per_world = scale.max(0.0001);
    let small_timer_factor = small_territory_timer_factor(sw, sh);
    let min_time_px = lerp_f32(10.0, DYNAMIC_TIME_MIN_PX, small_timer_factor);
    let min_cooldown_px = lerp_f32(8.8, DYNAMIC_COOLDOWN_MIN_PX, small_timer_factor);
    let min_time_world =
        (min_time_px / px_per_world).min(box_size * lerp_f32(0.35, 0.80, small_timer_factor));
    let min_cooldown_world =
        (min_cooldown_px / px_per_world).min(box_size * lerp_f32(0.50, 0.92, small_timer_factor));
    let time_floor = 6.0_f32.max(min_time_world);
    let time_cap = 44.0_f32.max(time_floor * 1.08);
    let cooldown_floor = DYNAMIC_COOLDOWN_MIN_WORLD.max(min_cooldown_world);
    let cooldown_cap = DYNAMIC_COOLDOWN_MAX_WORLD.max(cooldown_floor * 1.08);

    let tag_size = tag_size_base * dynamic_label_scale;
    let detail_size = (box_size * 0.125).clamp(5.2, 38.0) * dynamic_label_scale;
    let time_size_base =
        (box_size * DYNAMIC_TIME_SIZE_MULTIPLIER).clamp(time_floor, time_cap) * dynamic_label_scale;
    let time_size = if is_fresh {
        time_size_base
    } else {
        (time_size_base * DYNAMIC_TIME_STALE_SCALE).max(5.6)
    };
    let cooldown_size =
        (box_size * DYNAMIC_COOLDOWN_SIZE_MULTIPLIER * dynamic_cooldown_size_soften(box_size))
            .clamp(cooldown_floor, cooldown_cap)
            * dynamic_label_scale;
    let line_gap = (box_size * 0.048).clamp(2.0, 16.0) * dynamic_label_scale;

    Some(DynamicLabelSizing {
        small_timer_factor,
        tag_size,
        detail_size,
        time_size,
        cooldown_size,
        line_gap,
        time_max_width: timer_max_width_world(ww, small_timer_factor, DYNAMIC_TIME_MAX_WIDTH_BONUS),
        cooldown_max_width: timer_max_width_world(
            ww,
            small_timer_factor,
            DYNAMIC_COOLDOWN_MAX_WIDTH_BONUS,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{compute_dynamic_label_sizing, compute_static_label_sizing};

    fn assert_close(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-5,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn static_sizing_uses_base_tag_formula_at_overview_zoom() {
        let sizing = compute_static_label_sizing(180.0, 40.0, 0.2).expect("sizing should exist");
        assert_close(sizing.tag_size, 79.2);
    }

    #[test]
    fn dynamic_sizing_keeps_small_territory_readability_floors() {
        let sizing = compute_dynamic_label_sizing(112.0, 72.0, 0.25, 1.0, true)
            .expect("sizing should exist");
        assert!(sizing.tag_size > 72.0 * 0.236);
        assert!(sizing.time_size > 72.0 * 0.18);
    }

    #[test]
    fn larger_territories_only_scale_from_geometry_without_extra_boost() {
        let small = compute_dynamic_label_sizing(44.0, 40.0, 1.0, 1.0, true)
            .expect("small sizing should exist");
        let large = compute_dynamic_label_sizing(88.0, 80.0, 1.0, 1.0, true)
            .expect("large sizing should exist");

        assert_close(small.tag_size, 9.6);
        assert_close(large.tag_size, 18.88);
        assert!(large.tag_size > small.tag_size);
    }

    #[test]
    fn dynamic_sizing_is_deterministic() {
        let first = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, false)
            .expect("sizing should exist");
        let second = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, false)
            .expect("sizing should exist");
        assert_eq!(first, second);
    }
}
