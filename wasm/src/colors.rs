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
