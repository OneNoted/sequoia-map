#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) const STATIC_NAME_BASELINE_GAP_MULTIPLIER: f32 = 1.0;
pub(crate) const STATIC_NAME_MIN_RENDERED_PX: f32 = 12.0;

const STATIC_TAG_SIZE_WORLD: f32 = 24.0;
const STATIC_NAME_SIZE_WORLD: f32 = 21.5;
const FAR_ZOOM_TAG_MIN_SCREEN_W: f32 = 11.0;
const FAR_ZOOM_TAG_MIN_SCREEN_H: f32 = 8.0;
const FAR_ZOOM_TAG_MIN_RENDERED_PX: f32 = 7.5;
const FAR_ZOOM_TAG_MAX_RENDERED_PX: f32 = 12.0;
const FAR_ZOOM_TAG_BOX_W_FRACTION: f32 = 0.92;
const FAR_ZOOM_TAG_BOX_H_FRACTION: f32 = 0.82;
const DYNAMIC_TAG_SIZE_WORLD: f32 = 24.0;
const DYNAMIC_DETAIL_SIZE_WORLD: f32 = 21.5;
const DYNAMIC_TIME_SIZE_WORLD: f32 = 20.5;
const DYNAMIC_COOLDOWN_SIZE_WORLD: f32 = 23.0;
const DYNAMIC_LINE_GAP_WORLD: f32 = 6.0;
const DYNAMIC_TIME_MIN_WIDTH_WORLD: f32 = 108.0;
const DYNAMIC_COOLDOWN_MIN_WIDTH_WORLD: f32 = 132.0;
const DYNAMIC_TIME_STALE_SCALE: f32 = 0.96;
const RESOURCE_ICON_SIZE_WORLD: f32 = 29.0;
const RESOURCE_ICON_LABEL_LIFT_BOX_FRACTION: f32 = 0.22;
const RESOURCE_ICON_LABEL_LIFT_MAX_WORLD: f32 = 26.0;
const RESOURCE_ICON_LOWER_ANCHOR_START_RATIO: f32 = 0.62;
const RESOURCE_ICON_LOWER_ANCHOR_END_RATIO: f32 = 0.70;
const RESOURCE_ICON_EDGE_PADDING_WORLD: f32 = 3.0;
const ORNAMENT_INSET_WORLD: f32 = 3.0;
const ORNAMENT_CORNER_SHORT_SIDE_WORLD: f32 = 42.0;
const ORNAMENT_TINY_FIT_START_WORLD: f32 = 54.0;
const ORNAMENT_TINY_FIT_END_WORLD: f32 = 104.0;
const ORNAMENT_TINY_FIT_BOX_FRACTION: f32 = 0.48;
const ORNAMENT_SIZE_RAMP_START_WORLD: f32 = 54.0;
const ORNAMENT_SIZE_RAMP_END_WORLD: f32 = 220.0;
const ORNAMENT_SIZE_RAMP_MAX_SCALE: f32 = 1.65;
const ORNAMENT_TINT_ALPHA: f32 = 0.86;
const ORNAMENT_TINT_LIGHTEN_BASE: f32 = 0.04;
const ORNAMENT_TINT_LIGHTEN_DARK_BOOST: f32 = 0.12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StaticLabelSizing {
    pub detail_layout_alpha: f32,
    pub tag_size: f32,
    pub detail_size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FarZoomTagSizing {
    pub font_height_world: f32,
    pub max_width_world: f32,
    pub alpha: f32,
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

pub(crate) fn compute_static_label_sizing(ww: f32, hh: f32) -> Option<StaticLabelSizing> {
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

pub(crate) fn compute_far_zoom_tag_sizing(
    sw: f32,
    sh: f32,
    scale: f32,
    tag_scale: f32,
) -> Option<FarZoomTagSizing> {
    if !sw.is_finite() || !sh.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    if !tag_scale.is_finite() {
        return None;
    }
    if sw < FAR_ZOOM_TAG_MIN_SCREEN_W || sh < FAR_ZOOM_TAG_MIN_SCREEN_H {
        return None;
    }

    let max_font_px = sh * FAR_ZOOM_TAG_BOX_H_FRACTION;
    if max_font_px < FAR_ZOOM_TAG_MIN_RENDERED_PX {
        return None;
    }

    let scaled_font_px = FAR_ZOOM_TAG_MAX_RENDERED_PX * tag_scale.clamp(0.5, 2.0);
    let font_px = scaled_font_px
        .min(max_font_px)
        .clamp(FAR_ZOOM_TAG_MIN_RENDERED_PX, FAR_ZOOM_TAG_MAX_RENDERED_PX);
    let width_px = (sw * FAR_ZOOM_TAG_BOX_W_FRACTION).max(font_px * 1.6);
    let alpha = smoothstep_f32(8.0, 14.0, sh).min(smoothstep_f32(11.0, 22.0, sw));

    Some(FarZoomTagSizing {
        font_height_world: font_px / scale,
        max_width_world: width_px / scale,
        alpha,
    })
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn static_name_bottom_bound(
    use_static_gpu_labels: bool,
    static_show_names: bool,
    ww: f32,
    hh: f32,
    cy: f32,
    px_per_world: f32,
    tag_scale: f32,
    name_scale: f32,
    resource_icons_enabled: bool,
) -> Option<f32> {
    if !use_static_gpu_labels || !static_show_names {
        return None;
    }

    let sizing = compute_static_label_sizing(ww, hh)?;
    let detail_layout_alpha = sizing.detail_layout_alpha;
    if detail_layout_alpha <= 0.02 {
        return None;
    }
    let tag_size = sizing.tag_size * tag_scale.clamp(0.5, 4.0);
    let detail_size = sizing.detail_size * name_scale.clamp(0.5, 4.0);
    if detail_size * px_per_world < STATIC_NAME_MIN_RENDERED_PX {
        return None;
    }

    let label_lift =
        compute_resource_icon_label_lift_world(hh, detail_layout_alpha, resource_icons_enabled);
    let tag_y = lerp_f32(cy, cy - (detail_size + 1.0) * 0.45, detail_layout_alpha) - label_lift;
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

pub(crate) fn compute_resource_icon_label_lift_world(
    territory_height: f32,
    detail_layout_alpha: f32,
    resource_icons_enabled: bool,
) -> f32 {
    if !resource_icons_enabled {
        return 0.0;
    }

    let alpha = detail_layout_alpha.clamp(0.0, 1.0);
    if alpha <= 0.001 {
        return 0.0;
    }

    (territory_height.max(0.0) * RESOURCE_ICON_LABEL_LIFT_BOX_FRACTION)
        .min(RESOURCE_ICON_LABEL_LIFT_MAX_WORLD)
        * alpha
}

pub(crate) fn compute_resource_icon_center_y_world(
    territory_top: f32,
    territory_height: f32,
    detail_layout_alpha: f32,
    base_center_y: f32,
    icon_size_world: f32,
) -> Option<f32> {
    let territory_height = territory_height.max(0.0);
    let icon_half = icon_size_world.max(1.0) * 0.5;
    let top_limit = territory_top + icon_half + RESOURCE_ICON_EDGE_PADDING_WORLD;
    let bottom_limit =
        territory_top + territory_height - icon_half - RESOURCE_ICON_EDGE_PADDING_WORLD;
    if bottom_limit <= top_limit {
        return None;
    }

    let lower_anchor = territory_top
        + territory_height
            * lerp_f32(
                RESOURCE_ICON_LOWER_ANCHOR_START_RATIO,
                RESOURCE_ICON_LOWER_ANCHOR_END_RATIO,
                detail_layout_alpha,
            );
    let desired_center_y = base_center_y.max(lower_anchor).max(top_limit);
    if desired_center_y > bottom_limit {
        return None;
    }
    Some(desired_center_y)
}

pub(crate) fn compute_territory_ornament_sizing(
    ww: f32,
    hh: f32,
    ornament_aspect: f32,
    icon_scale: f32,
) -> TerritoryOrnamentSizing {
    let territory_short_side = ww.min(hh).max(1.0);
    let size_ramp = smoothstep_f32(
        ORNAMENT_SIZE_RAMP_START_WORLD,
        ORNAMENT_SIZE_RAMP_END_WORLD,
        territory_short_side,
    );
    let short_side_world = (ORNAMENT_CORNER_SHORT_SIDE_WORLD
        * lerp_f32(1.0, ORNAMENT_SIZE_RAMP_MAX_SCALE, size_ramp)
        * icon_scale.max(0.0))
    .max(1.0);
    let aspect = ornament_aspect.max(0.01);
    let (corner_w_world, corner_h_world) = if aspect >= 1.0 {
        (short_side_world * aspect, short_side_world)
    } else {
        (short_side_world, short_side_world / aspect)
    };
    let available_w = (ww - ORNAMENT_INSET_WORLD * 2.0).max(2.0);
    let available_h = (hh - ORNAMENT_INSET_WORLD * 2.0).max(2.0);
    let fit_w = ((available_w * ORNAMENT_TINY_FIT_BOX_FRACTION) / corner_w_world.max(0.001))
        .clamp(0.0, 1.0);
    let fit_h = ((available_h * ORNAMENT_TINY_FIT_BOX_FRACTION) / corner_h_world.max(0.001))
        .clamp(0.0, 1.0);
    let pair_fit_w = ((available_w * 0.5) / corner_w_world.max(0.001)).clamp(0.0, 1.0);
    let pair_fit_h = ((available_h * 0.5) / corner_h_world.max(0.001)).clamp(0.0, 1.0);
    let tiny_fit = 1.0
        - smoothstep_f32(
            ORNAMENT_TINY_FIT_START_WORLD,
            ORNAMENT_TINY_FIT_END_WORLD,
            territory_short_side,
        );
    let fit_scale = lerp_f32(1.0, fit_w.min(fit_h), tiny_fit).min(pair_fit_w.min(pair_fit_h));
    TerritoryOrnamentSizing {
        inset_world: ORNAMENT_INSET_WORLD,
        corner_w_world: corner_w_world * fit_scale,
        corner_h_world: corner_h_world * fit_scale,
    }
}

pub(crate) fn compute_territory_ornament_tint(guild_rgb: (u8, u8, u8)) -> [f32; 4] {
    let (r, g, b) = guild_rgb;
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;
    let luminance = 0.299 * rf + 0.587 * gf + 0.114 * bf;
    let dark_boost = (1.0 - luminance).clamp(0.0, 1.0);
    let lighten = ORNAMENT_TINT_LIGHTEN_BASE + dark_boost * ORNAMENT_TINT_LIGHTEN_DARK_BOOST;
    [
        lerp_f32(rf, 1.0, lighten),
        lerp_f32(gf, 1.0, lighten),
        lerp_f32(bf, 1.0, lighten),
        ORNAMENT_TINT_ALPHA,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        compute_dynamic_label_sizing, compute_far_zoom_tag_sizing,
        compute_resource_icon_center_y_world, compute_resource_icon_label_lift_world,
        compute_resource_icon_size_world, compute_static_label_sizing,
        compute_territory_ornament_sizing, compute_territory_ornament_tint,
    };

    fn assert_close(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-4,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn static_sizing_is_fixed_in_world_space() {
        let small = compute_static_label_sizing(24.0, 16.0).expect("small sizing should exist");
        let large = compute_static_label_sizing(180.0, 40.0).expect("large sizing should exist");
        assert_close(small.tag_size, 24.0);
        assert_close(large.tag_size, 24.0);
        assert_close(small.detail_size, 21.5);
        assert_close(large.detail_size, 21.5);
    }

    #[test]
    fn far_zoom_tag_sizing_is_screen_stable() {
        let near = compute_far_zoom_tag_sizing(42.0, 18.0, 0.60, 1.0).expect("sizing should exist");
        let far = compute_far_zoom_tag_sizing(42.0, 18.0, 0.30, 1.0).expect("sizing should exist");

        assert_close(near.font_height_world * 0.60, far.font_height_world * 0.30);
        assert_close(near.max_width_world * 0.60, far.max_width_world * 0.30);
    }

    #[test]
    fn far_zoom_tag_sizing_hides_impossible_boxes() {
        assert!(compute_far_zoom_tag_sizing(9.0, 18.0, 0.30, 1.0).is_none());
        assert!(compute_far_zoom_tag_sizing(42.0, 7.0, 0.30, 1.0).is_none());
    }

    #[test]
    fn far_zoom_tag_sizing_rejects_non_finite_tag_scale() {
        assert!(compute_far_zoom_tag_sizing(42.0, 18.0, 0.30, f32::NAN).is_none());
        assert!(compute_far_zoom_tag_sizing(42.0, 18.0, 0.30, f32::INFINITY).is_none());
    }

    #[test]
    fn far_zoom_tag_sizing_keeps_small_readable_boxes() {
        let sizing = compute_far_zoom_tag_sizing(18.0, 10.0, 0.30, 1.0)
            .expect("small readable territory should keep a tag");

        assert!(sizing.font_height_world * 0.30 >= 7.5);
        assert!(sizing.max_width_world * 0.30 >= 16.5);
        assert!(sizing.alpha > 0.0);
    }

    #[test]
    fn dynamic_sizing_is_fixed_across_territories_and_zoom() {
        let near = compute_dynamic_label_sizing(88.0, 80.0, 1.0, 1.0, true)
            .expect("near sizing should exist");
        let far = compute_dynamic_label_sizing(160.0, 90.0, 0.35, 1.0, true)
            .expect("far sizing should exist");
        let other_territory = compute_dynamic_label_sizing(44.0, 40.0, 1.0, 1.0, true)
            .expect("other territory sizing should exist");
        assert_close(near.tag_size, 24.0);
        assert_close(far.tag_size, 24.0);
        assert_close(other_territory.tag_size, 24.0);
        assert_close(near.time_size, 20.5);
        assert_close(far.time_size, 20.5);
        assert_close(other_territory.time_size, 20.5);
    }

    #[test]
    fn dynamic_sizing_uses_fixed_world_widths() {
        let sizing =
            compute_dynamic_label_sizing(44.0, 40.0, 1.0, 1.0, true).expect("sizing should exist");
        assert_close(sizing.time_max_width, 108.0);
        assert_close(sizing.cooldown_max_width, 132.0);
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
        assert_close(size, 29.0);
    }

    #[test]
    fn resource_icon_label_lift_moves_labels_toward_upper_quadrant() {
        let lift = compute_resource_icon_label_lift_world(80.0, 1.0, true);
        assert_close(lift, 17.6);
        assert_close(compute_resource_icon_label_lift_world(80.0, 0.5, true), 8.8);
        assert_close(
            compute_resource_icon_label_lift_world(80.0, 1.0, false),
            0.0,
        );
    }

    #[test]
    fn resource_icon_label_lift_is_capped_for_large_territories() {
        let lift = compute_resource_icon_label_lift_world(220.0, 1.0, true);
        assert_close(lift, 26.0);
    }

    #[test]
    fn resource_icon_center_prefers_lower_territory_band() {
        let center = compute_resource_icon_center_y_world(100.0, 120.0, 1.0, 145.0, 29.0)
            .expect("icon should fit");
        assert_close(center, 184.0);
    }

    #[test]
    fn resource_icon_center_hides_when_vertical_space_would_overlap_labels() {
        let center = compute_resource_icon_center_y_world(100.0, 70.0, 1.0, 260.0, 29.0);
        assert!(center.is_none());
    }

    #[test]
    fn territory_ornament_size_grows_with_territory_size() {
        let small = compute_territory_ornament_sizing(48.0, 48.0, 1.0, 1.0);
        let medium = compute_territory_ornament_sizing(120.0, 120.0, 1.0, 1.0);
        let large = compute_territory_ornament_sizing(260.0, 260.0, 1.0, 1.0);
        let wide = compute_territory_ornament_sizing(260.0, 260.0, 2.0, 1.0);
        let scaled = compute_territory_ornament_sizing(260.0, 260.0, 1.0, 1.5);

        assert_close(small.inset_world, 3.0);
        assert!(medium.corner_w_world > small.corner_w_world);
        assert!(medium.corner_h_world > small.corner_h_world);
        assert!(small.corner_w_world <= (48.0 - small.inset_world * 2.0) * 0.5);
        assert!(small.corner_h_world <= (48.0 - small.inset_world * 2.0) * 0.5);
        assert_close(large.corner_w_world, 69.3);
        assert_close(large.corner_h_world, 69.3);
        assert_close(wide.corner_w_world, 127.0);
        assert_close(wide.corner_h_world, 63.5);
        assert_close(scaled.corner_w_world, 103.95);
        assert_close(scaled.corner_h_world, 103.95);
    }

    #[test]
    fn territory_ornament_shrinks_wide_borders_to_fit_small_territories() {
        let wide = compute_territory_ornament_sizing(120.0, 120.0, 2.0, 1.0);
        let available = 120.0 - wide.inset_world * 2.0;

        assert!(wide.corner_w_world > 0.0);
        assert!(wide.corner_h_world > 0.0);
        assert!(wide.corner_w_world <= available * 0.5);
        assert!(wide.corner_h_world <= available * 0.5);
    }

    #[test]
    fn territory_ornament_tint_tracks_territory_color() {
        let tint = compute_territory_ornament_tint((64, 128, 224));
        let base_r = 64.0 / 255.0;
        let base_g = 128.0 / 255.0;
        let base_b = 224.0 / 255.0;

        assert!(tint[0] > base_r);
        assert!(tint[1] > base_g);
        assert!(tint[2] > base_b);
        assert!(tint[2] > tint[1]);
        assert!(tint[1] > tint[0]);
        assert_close(tint[3], 0.86);
    }
}
