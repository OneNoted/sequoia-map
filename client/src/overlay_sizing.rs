#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) const STATIC_NAME_BASELINE_GAP_MULTIPLIER: f32 = 1.0;

const STATIC_TAG_SIZE_WORLD: f32 = 15.0;
const STATIC_NAME_SIZE_WORLD: f32 = 13.5;
const DYNAMIC_TAG_SIZE_WORLD: f32 = 15.0;
const DYNAMIC_DETAIL_SIZE_WORLD: f32 = 13.5;
const DYNAMIC_TIME_SIZE_WORLD: f32 = 13.0;
const DYNAMIC_COOLDOWN_SIZE_WORLD: f32 = 14.5;
const DYNAMIC_LINE_GAP_WORLD: f32 = 4.0;
const DYNAMIC_TIME_MIN_WIDTH_WORLD: f32 = 68.0;
const DYNAMIC_COOLDOWN_MIN_WIDTH_WORLD: f32 = 82.0;
const DYNAMIC_TIME_STALE_SCALE: f32 = 0.96;
const RESOURCE_ICON_SIZE_WORLD: f32 = 18.0;
const ORNAMENT_INSET_WORLD: f32 = 2.0;
const ORNAMENT_CORNER_SHORT_SIDE_WORLD: f32 = 26.0;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerritoryOrnamentSizing {
    pub inset_world: f32,
    pub corner_w_world: f32,
    pub corner_h_world: f32,
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
fn static_label_visible(ww: f32, hh: f32) -> bool {
    if ww < 8.0 || hh < 6.0 {
        return false;
    }
    true
}

#[inline]
fn dynamic_label_visible(sw: f32, sh: f32) -> bool {
    if sw < 10.0 || sh < 8.0 {
        return false;
    }
    if sw < 28.0 || sh < 18.0 {
        return false;
    }
    true
}

#[inline]
fn timer_max_width_world(ww: f32, min_width: f32) -> f32 {
    (ww - 8.0).max(min_width)
}

pub(crate) fn compute_static_label_sizing(
    ww: f32,
    hh: f32,
    _scale: f32,
) -> Option<StaticLabelSizing> {
    if !static_label_visible(ww, hh) {
        return None;
    }
    let detail_layout_x = smoothstep_f32(14.0, 36.0, ww);
    let detail_layout_y = smoothstep_f32(9.0, 24.0, hh);
    let detail_layout_alpha = (detail_layout_x * detail_layout_y).sqrt();

    Some(StaticLabelSizing {
        detail_layout_alpha,
        tag_size: STATIC_TAG_SIZE_WORLD,
        detail_size: STATIC_NAME_SIZE_WORLD,
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
    if !dynamic_label_visible(sw, sh) {
        return None;
    }

    let small_timer_factor = 0.0;
    let tag_size = DYNAMIC_TAG_SIZE_WORLD * dynamic_label_scale;
    let detail_size = DYNAMIC_DETAIL_SIZE_WORLD * dynamic_label_scale;
    let time_size_base = DYNAMIC_TIME_SIZE_WORLD * dynamic_label_scale;
    let time_size = if is_fresh {
        time_size_base
    } else {
        (time_size_base * DYNAMIC_TIME_STALE_SCALE).max(5.6)
    };
    let cooldown_size = DYNAMIC_COOLDOWN_SIZE_WORLD * dynamic_label_scale;
    let line_gap = DYNAMIC_LINE_GAP_WORLD * dynamic_label_scale;

    Some(DynamicLabelSizing {
        small_timer_factor,
        tag_size,
        detail_size,
        time_size,
        cooldown_size,
        line_gap,
        time_max_width: timer_max_width_world(ww, DYNAMIC_TIME_MIN_WIDTH_WORLD),
        cooldown_max_width: timer_max_width_world(ww, DYNAMIC_COOLDOWN_MIN_WIDTH_WORLD),
    })
}

pub(crate) fn compute_resource_icon_size_world(icon_scale: f32) -> f32 {
    (RESOURCE_ICON_SIZE_WORLD * icon_scale.max(0.0)).max(1.0)
}

pub(crate) fn compute_territory_ornament_sizing(
    ornament_aspect: f32,
    icon_scale: f32,
) -> TerritoryOrnamentSizing {
    let short_side_world = (ORNAMENT_CORNER_SHORT_SIDE_WORLD * icon_scale.max(0.0)).max(1.0);
    let aspect = ornament_aspect.max(0.01);
    let (corner_w_world, corner_h_world) = if aspect >= 1.0 {
        (short_side_world * aspect, short_side_world)
    } else {
        (short_side_world, short_side_world / aspect)
    };
    TerritoryOrnamentSizing {
        inset_world: ORNAMENT_INSET_WORLD,
        corner_w_world,
        corner_h_world,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_dynamic_label_sizing, compute_resource_icon_size_world,
        compute_static_label_sizing, compute_territory_ornament_sizing,
    };

    fn assert_close(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-5,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn static_sizing_is_fixed_in_world_space() {
        let small =
            compute_static_label_sizing(24.0, 16.0, 0.2).expect("small sizing should exist");
        let large =
            compute_static_label_sizing(180.0, 40.0, 1.0).expect("large sizing should exist");
        assert_close(small.tag_size, 15.0);
        assert_close(large.tag_size, 15.0);
        assert_close(small.detail_size, 13.5);
        assert_close(large.detail_size, 13.5);
    }

    #[test]
    fn dynamic_sizing_is_fixed_across_territories_and_zoom() {
        let near = compute_dynamic_label_sizing(88.0, 80.0, 1.0, 1.0, true)
            .expect("near sizing should exist");
        let far = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, true)
            .expect("far sizing should exist");
        let other_territory = compute_dynamic_label_sizing(44.0, 40.0, 1.0, 1.0, true)
            .expect("other territory sizing should exist");
        assert_close(near.tag_size, 15.0);
        assert_close(far.tag_size, 15.0);
        assert_close(other_territory.tag_size, 15.0);
        assert_close(near.time_size, 13.0);
        assert_close(far.time_size, 13.0);
        assert_close(other_territory.time_size, 13.0);
    }

    #[test]
    fn dynamic_sizing_uses_fixed_world_widths() {
        let sizing =
            compute_dynamic_label_sizing(44.0, 40.0, 1.0, 1.0, true).expect("sizing should exist");
        assert_close(sizing.time_max_width, 68.0);
        assert_close(sizing.cooldown_max_width, 82.0);
    }

    #[test]
    fn dynamic_sizing_still_hides_when_territory_is_too_small() {
        let sizing = compute_dynamic_label_sizing(20.0, 18.0, 1.0, 1.0, true);
        assert!(sizing.is_none());
    }

    #[test]
    fn dynamic_sizing_is_deterministic() {
        let first = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, false)
            .expect("sizing should exist");
        let second = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, false)
            .expect("sizing should exist");
        assert_eq!(first, second);
    }

    #[test]
    fn resource_icon_size_is_fixed_in_world_space() {
        let size = compute_resource_icon_size_world(1.0);
        assert_close(size, 18.0);
    }

    #[test]
    fn territory_ornament_size_is_fixed_in_world_space() {
        let square = compute_territory_ornament_sizing(1.0, 1.0);
        let wide = compute_territory_ornament_sizing(2.0, 1.0);
        let scaled = compute_territory_ornament_sizing(1.0, 1.5);

        assert_close(square.inset_world, 2.0);
        assert_close(square.corner_w_world, 26.0);
        assert_close(square.corner_h_world, 26.0);
        assert_close(wide.corner_w_world, 52.0);
        assert_close(wide.corner_h_world, 26.0);
        assert_close(scaled.corner_w_world, 39.0);
        assert_close(scaled.corner_h_world, 39.0);
    }
}
