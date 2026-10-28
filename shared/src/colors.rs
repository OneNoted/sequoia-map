/// Deterministic guild color via CRC32 hash of guild name.
/// Returns (r, g, b) from first 3 bytes of hash.
pub fn guild_color(name: &str) -> (u8, u8, u8) {
    let hash = crc32fast::hash(name.as_bytes());
    let bytes = hash.to_be_bytes();
    (bytes[0], bytes[1], bytes[2])
}

/// Convert RGB to HSL. Returns (h: 0..360, s: 0..1, l: 0..1).
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let r = r as f64 / 255.0;
    let g = g as f64 / 255.0;
    let b = b as f64 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < f64::EPSILON {
        let mut h = (g - b) / d;
        if g < b {
            h += 6.0;
        }
        h
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };

    (h * 60.0, s, l)
}

/// Convert HSL to RGB.
pub fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    if s.abs() < f64::EPSILON {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let h = h / 360.0;

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

/// Interpolate between two HSL colors using shortest hue path.
pub fn interpolate_hsl(from: (f64, f64, f64), to: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    let mut dh = to.0 - from.0;
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }

    let h = (from.0 + dh * t).rem_euclid(360.0);
    let s = from.1 + (to.1 - from.1) * t;
    let l = from.2 + (to.2 - from.2) * t;

    (h, s, l)
}

#[cfg(test)]
mod tests {
    use super::{guild_color, hsl_to_rgb, interpolate_hsl, rgb_to_hsl};
    use crate::test_support::assert_close;

    #[test]
    fn roundtrip_rgb_through_hsl_is_identity() {
        let samples = [
            (0, 0, 0),
            (255, 255, 255),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (37, 91, 201),
            (250, 180, 20),
        ];

        for (r, g, b) in samples {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            assert_eq!(hsl_to_rgb(h, s, l), (r, g, b));
        }
    }

    #[test]
    fn rgb_to_hsl_gray_has_zero_saturation() {
        let (h, s, l) = rgb_to_hsl(128, 128, 128);
        assert_close(h, 0.0);
        assert_close(s, 0.0);
        assert_close(l, 128.0 / 255.0);
    }

    #[test]
    fn rgb_to_hsl_pure_primaries() {
        let (h_r, s_r, l_r) = rgb_to_hsl(255, 0, 0);
        assert_close(h_r, 0.0);
        assert_close(s_r, 1.0);
        assert_close(l_r, 0.5);

        let (h_g, s_g, l_g) = rgb_to_hsl(0, 255, 0);
        assert_close(h_g, 120.0);
        assert_close(s_g, 1.0);
        assert_close(l_g, 0.5);

        let (h_b, s_b, l_b) = rgb_to_hsl(0, 0, 255);
        assert_close(h_b, 240.0);
        assert_close(s_b, 1.0);
        assert_close(l_b, 0.5);
    }

    #[test]
    fn interpolate_hsl_wraps_shortest_path() {
        let from = (350.0, 0.6, 0.4);
        let to = (10.0, 0.8, 0.5);

        let mid = interpolate_hsl(from, to, 0.5);
        assert_close(mid.0, 0.0);
        assert_close(mid.1, 0.7);
        assert_close(mid.2, 0.45);
    }

    #[test]
    fn interpolate_hsl_at_t0_and_t1() {
        let from = (42.0, 0.1, 0.2);
        let to = (300.0, 0.9, 0.8);

        assert_eq!(interpolate_hsl(from, to, 0.0), from);
        assert_eq!(interpolate_hsl(from, to, 1.0), to);
    }

    #[test]
    fn guild_color_varies_for_different_names() {
        assert_ne!(guild_color("The Hive"), guild_color("Canyon Condors"));
    }
}
