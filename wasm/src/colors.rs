/// Format RGBA as a CSS color string.
pub fn rgba_css(r: u8, g: u8, b: u8, a: f64) -> String {
    format!("rgba({r},{g},{b},{a})")
}

/// Brighten a color by a factor (1.0 = no change, >1.0 = brighter).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn brighten(r: u8, g: u8, b: u8, factor: f64) -> (u8, u8, u8) {
    (
        ((r as f64 * factor).min(255.0)) as u8,
        ((g as f64 * factor).min(255.0)) as u8,
        ((b as f64 * factor).min(255.0)) as u8,
    )
}

fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    let t = t.clamp(0.0, 1.0);
    let value = a as f64 + (b as f64 - a as f64) * t;
    value.round().clamp(0.0, 255.0) as u8
}

pub fn heat_color_for_intensity(intensity: f64) -> (u8, u8, u8) {
    const STOPS: &[(f64, (u8, u8, u8))] = &[
        (0.00, (30, 80, 220)),
        (0.25, (40, 200, 240)),
        (0.50, (245, 220, 70)),
        (0.75, (245, 140, 50)),
        (1.00, (220, 40, 35)),
    ];

    let intensity = intensity.clamp(0.0, 1.0);
    for window in STOPS.windows(2) {
        let (left_pos, left_color) = window[0];
        let (right_pos, right_color) = window[1];
        if intensity >= left_pos && intensity <= right_pos {
            let span = (right_pos - left_pos).max(f64::EPSILON);
            let t = (intensity - left_pos) / span;
            return (
                lerp_u8(left_color.0, right_color.0, t),
                lerp_u8(left_color.1, right_color.1, t),
                lerp_u8(left_color.2, right_color.2, t),
            );
        }
    }

    STOPS
        .last()
        .map(|(_, color)| *color)
        .unwrap_or((220, 40, 35))
}

pub fn heat_color_for_count(take_count: u64, max_take_count: u64) -> (u8, u8, u8) {
    if max_take_count == 0 {
        return heat_color_for_intensity(0.0);
    }
    let intensity = (take_count as f64 / max_take_count as f64).clamp(0.0, 1.0);
    heat_color_for_intensity(intensity)
}

#[cfg(test)]
mod heat_tests {
    use super::*;
    #[test]
    fn heat_color_handles_zero_max() {
        assert_eq!(heat_color_for_count(0, 0), (30, 80, 220));
        assert_eq!(heat_color_for_count(10, 0), (30, 80, 220));
    }
    #[test]
    fn heat_color_matches_gradient_edges() {
        assert_eq!(heat_color_for_intensity(0.0), (30, 80, 220));
        assert_eq!(heat_color_for_intensity(0.5), (245, 220, 70));
        assert_eq!(heat_color_for_intensity(1.0), (220, 40, 35));
    }
}
