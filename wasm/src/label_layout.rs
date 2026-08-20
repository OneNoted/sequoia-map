#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use std::fmt::Write;

use sequoia_shared::Resources;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconKind {
    Emerald,
    Ore,
    Crops,
    Fish,
    Wood,
    Rainbow,
    HqCrown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelLayoutMetrics {
    pub detail_layout_alpha: f32,
    pub tag_y_ratio: f32,
    pub name_y_ratio: f32,
    pub time_y_ratio: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct DynamicTextState {
    pub age_secs: i64,
    pub is_fresh: bool,
    pub cooldown_frac: f32,
}

pub fn abbreviate_name(name: &str) -> String {
    if !name.contains(' ') {
        return name.to_string();
    }
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .collect()
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn format_age(age_secs: i64) -> String {
    let mut out = String::with_capacity(8);
    write_age(&mut out, age_secs);
    out
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn format_age_compound(age_secs: i64) -> String {
    let mut out = String::with_capacity(8);
    write_age_compound(&mut out, age_secs);
    out
}

pub fn write_age(buf: &mut String, age_secs: i64) {
    buf.clear();
    if age_secs < 60 {
        buf.push_str("now");
    } else if age_secs < 600 {
        let _ = write!(buf, "{}:{:02}", age_secs / 60, age_secs % 60);
    } else if age_secs < 3600 {
        let _ = write!(buf, "{}m", age_secs / 60);
    } else if age_secs < 86400 {
        let _ = write!(buf, "{}h", age_secs / 3600);
    } else if age_secs < 604800 {
        let _ = write!(buf, "{}d", age_secs / 86400);
    } else {
        let _ = write!(buf, "{}w", age_secs / 604800);
    }
}

pub fn write_age_compound(buf: &mut String, age_secs: i64) {
    buf.clear();
    let secs = age_secs.max(0);
    if secs >= 86400 {
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        let _ = write!(buf, "{days}d{hours}h");
    } else if secs >= 3600 {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        let _ = write!(buf, "{hours}h{minutes:02}m");
    } else {
        let minutes = secs / 60;
        let seconds = secs % 60;
        let _ = write!(buf, "{minutes}m{seconds:02}s");
    }
}

pub fn cooldown_color(urgency: f64) -> (u8, u8, u8) {
    if urgency < 0.25 {
        (102, 204, 102)
    } else if urgency < 0.50 {
        (245, 197, 66)
    } else if urgency < 0.75 {
        (245, 158, 66)
    } else {
        (235, 87, 87)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn dynamic_text_state(reference_time_secs: i64, acquired_secs: i64) -> DynamicTextState {
    let age_secs = (reference_time_secs - acquired_secs).max(0);
    let is_fresh = age_secs < 600;
    let cooldown_frac = if is_fresh {
        ((600 - age_secs) as f32 / 600.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    DynamicTextState {
        age_secs,
        is_fresh,
        cooldown_frac,
    }
}

#[inline]
fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn compute_label_layout_metrics(
    sw: f64,
    sh: f64,
    classic_static_layout: bool,
) -> LabelLayoutMetrics {
    let detail_layout_alpha = if classic_static_layout {
        if sw >= 36.0 && sh >= 24.0 { 1.0 } else { 0.0 }
    } else {
        let detail_layout_x = smoothstep(16.0, 36.0, sw);
        let detail_layout_y = smoothstep(10.0, 28.0, sh);
        (detail_layout_x * detail_layout_y).sqrt()
    };
    let tag_y_ratio = if classic_static_layout {
        0.44
    } else {
        // Default to center-ish tag placement in dynamic mode.
        0.50
    };
    let name_y_ratio = if classic_static_layout { 0.60 } else { 0.62 };
    let time_y_ratio = if classic_static_layout { 0.73 } else { 0.74 };
    LabelLayoutMetrics {
        detail_layout_alpha: detail_layout_alpha as f32,
        tag_y_ratio: tag_y_ratio as f32,
        name_y_ratio: name_y_ratio as f32,
        time_y_ratio: time_y_ratio as f32,
    }
}

pub fn resource_icon_sequence(resources: &Resources) -> Vec<IconKind> {
    if resources.has_all() {
        return vec![IconKind::Rainbow];
    }

    let mut out = Vec::with_capacity(10);
    if resources.has_double_emeralds() {
        out.push(IconKind::Emerald);
    }
    if resources.ore > 0 {
        out.push(IconKind::Ore);
        if resources.has_double_ore() {
            out.push(IconKind::Ore);
        }
    }
    if resources.crops > 0 {
        out.push(IconKind::Crops);
        if resources.has_double_crops() {
            out.push(IconKind::Crops);
        }
    }
    if resources.fish > 0 {
        out.push(IconKind::Fish);
        if resources.has_double_fish() {
            out.push(IconKind::Fish);
        }
    }
    if resources.wood > 0 {
        out.push(IconKind::Wood);
        if resources.has_double_wood() {
            out.push(IconKind::Wood);
        }
    }
    out
}

pub fn resource_icons_drawable(resources: &Resources) -> bool {
    resources.has_all()
        || resources.has_double_emeralds()
        || resources.ore > 0
        || resources.crops > 0
        || resources.fish > 0
        || resources.wood > 0
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn dynamic_label_next_update_age(
    age_secs: i64,
    show_countdown: bool,
    granular_time: bool,
    compound_time: bool,
) -> i64 {
    if show_countdown || granular_time || age_secs < 600 {
        return age_secs + 1;
    }
    if compound_time {
        if age_secs < 3600 {
            return age_secs + 1;
        }
        if age_secs < 86400 {
            return ((age_secs / 60) + 1) * 60;
        }
        return ((age_secs / 3600) + 1) * 3600;
    }
    if age_secs < 3600 {
        return ((age_secs / 60) + 1) * 60;
    }
    if age_secs < 86400 {
        return ((age_secs / 3600) + 1) * 3600;
    }
    if age_secs < 604800 {
        return ((age_secs / 86400) + 1) * 86400;
    }
    ((age_secs / 604800) + 1) * 604800
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-6,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn test_abbreviate_name() {
        assert_eq!(abbreviate_name("Ragni"), "Ragni");
        assert_eq!(abbreviate_name("Cascading Basins"), "CB");
        assert_eq!(abbreviate_name("The Forgery"), "TF");
    }

    #[test]
    fn test_format_age_boundaries() {
        assert_eq!(format_age(59), "now");
        assert_eq!(format_age(60), "1:00");
        assert_eq!(format_age(599), "9:59");
        assert_eq!(format_age(600), "10m");
        assert_eq!(format_age(3599), "59m");
        assert_eq!(format_age(3600), "1h");
        assert_eq!(format_age(86_399), "23h");
        assert_eq!(format_age(86_400), "1d");
        assert_eq!(format_age(604_799), "6d");
        assert_eq!(format_age(604_800), "1w");
    }

    #[test]
    fn test_format_age_compound_examples() {
        assert_eq!(format_age_compound(5 * 86_400 + 23 * 3_600), "5d23h");
        assert_eq!(format_age_compound(18 * 3_600 + 50 * 60), "18h50m");
        assert_eq!(format_age_compound(46 * 60 + 20), "46m20s");
        assert_eq!(format_age_compound(-8), "0m00s");
    }

    #[test]
    fn test_dynamic_label_next_update_age_compound_mode() {
        assert_eq!(
            dynamic_label_next_update_age(3_599, false, false, true),
            3_600
        );
        assert_eq!(
            dynamic_label_next_update_age(3_600, false, false, true),
            3_660
        );
        assert_eq!(
            dynamic_label_next_update_age(86_399, false, false, true),
            86_400
        );
        assert_eq!(
            dynamic_label_next_update_age(86_400, false, false, true),
            90_000
        );
    }

    #[test]
    fn cooldown_color_at_each_threshold_boundary() {
        assert_eq!(cooldown_color(0.0), (102, 204, 102));
        assert_eq!(cooldown_color(0.24), (102, 204, 102));
        assert_eq!(cooldown_color(0.25), (245, 197, 66));
        assert_eq!(cooldown_color(0.49), (245, 197, 66));
        assert_eq!(cooldown_color(0.50), (245, 158, 66));
        assert_eq!(cooldown_color(0.74), (245, 158, 66));
        assert_eq!(cooldown_color(0.75), (235, 87, 87));
        assert_eq!(cooldown_color(1.0), (235, 87, 87));
    }

    #[test]
    fn dynamic_text_state_fresh_within_600s() {
        let state = dynamic_text_state(1_000, 500);
        assert_eq!(state.age_secs, 500);
        assert!(state.is_fresh);
        assert_close(state.cooldown_frac, 100.0 / 600.0);
    }

    #[test]
    fn dynamic_text_state_not_fresh_after_600s() {
        let state = dynamic_text_state(1_000, 400);
        assert_eq!(state.age_secs, 600);
        assert!(!state.is_fresh);
        assert_close(state.cooldown_frac, 0.0);
    }

    #[test]
    fn smoothstep_at_edges() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 1.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smoothstep(1.0, 1.0, 0.5), 0.0);
        assert_eq!(smoothstep(1.0, 1.0, 1.0), 1.0);
    }

    #[test]
    fn compute_label_layout_metrics_classic_vs_dynamic() {
        let classic = compute_label_layout_metrics(20.0, 15.0, true);
        let dynamic = compute_label_layout_metrics(20.0, 15.0, false);

        assert_eq!(classic.detail_layout_alpha, 0.0);
        assert!(dynamic.detail_layout_alpha > classic.detail_layout_alpha);
        assert!(dynamic.detail_layout_alpha < 1.0);

        assert_eq!(classic.tag_y_ratio, 0.44);
        assert_eq!(classic.name_y_ratio, 0.60);
        assert_eq!(classic.time_y_ratio, 0.73);

        assert_eq!(dynamic.tag_y_ratio, 0.50);
        assert_eq!(dynamic.name_y_ratio, 0.62);
        assert_eq!(dynamic.time_y_ratio, 0.74);
    }

    #[test]
    fn test_dynamic_label_next_update_age() {
        assert_eq!(dynamic_label_next_update_age(5, true, false, false), 6);
        assert_eq!(dynamic_label_next_update_age(610, false, false, false), 660);
        assert_eq!(
            dynamic_label_next_update_age(3690, false, false, false),
            7200
        );
    }

    #[test]
    fn test_resource_icon_sequence() {
        let resources = Resources {
            ore: 7200,
            fish: 2000,
            ..Resources::default()
        };
        let seq = resource_icon_sequence(&resources);
        assert_eq!(seq, vec![IconKind::Ore, IconKind::Ore, IconKind::Fish]);
    }

    #[test]
    fn test_resource_icon_sequence_omits_single_emerald() {
        let mut resources = Resources {
            emeralds: 9000,
            ..Resources::default()
        };
        assert!(resource_icon_sequence(&resources).is_empty());
        assert!(!resource_icons_drawable(&resources));

        resources.emeralds = 18000;
        assert_eq!(resource_icon_sequence(&resources), vec![IconKind::Emerald]);
        assert!(resource_icons_drawable(&resources));
    }

    #[test]
    fn test_resource_icon_sequence_all() {
        let resources = Resources {
            emeralds: 10_000,
            ore: 100,
            crops: 100,
            fish: 100,
            wood: 100,
        };
        assert_eq!(resource_icon_sequence(&resources), vec![IconKind::Rainbow]);
        assert!(resource_icons_drawable(&resources));
    }

    #[test]
    fn test_resource_icons_drawable_for_non_emerald_resources() {
        let resources = Resources {
            ore: 3600,
            ..Resources::default()
        };
        assert_eq!(resource_icon_sequence(&resources), vec![IconKind::Ore]);
        assert!(resource_icons_drawable(&resources));
    }

    #[test]
    fn test_layout_determinism() {
        let a = compute_label_layout_metrics(72.0, 45.0, false);
        let b = compute_label_layout_metrics(72.0, 45.0, false);
        assert_eq!(a, b);
    }
}
