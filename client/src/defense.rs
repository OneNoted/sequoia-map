pub(crate) const DEFENSE_TIERS: &[(&str, &str)] = &[
    ("VERY LOW", "#00aa00"),
    ("LOW", "#55ff55"),
    ("MEDIUM", "#ffff55"),
    ("HIGH", "#ff5555"),
    ("VERY HIGH", "#aa0000"),
];

pub(crate) fn normalize_defense_tier(tier: &str) -> String {
    tier.trim()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

pub(crate) fn defense_tier_display(tier: &str) -> (String, &'static str) {
    let normalized = normalize_defense_tier(tier);
    match normalized.as_str() {
        "NONE" => (normalized, "#ffffff"),
        "VERY LOW" => (normalized, "#00aa00"),
        "LOW" => (normalized, "#55ff55"),
        "MEDIUM" => (normalized, "#ffff55"),
        "HIGH" => (normalized, "#ff5555"),
        "VERY HIGH" => (normalized, "#aa0000"),
        _ => (tier.to_string(), "#e2e0d8"),
    }
}

pub(crate) fn defense_tier_overlay_data(tier: Option<&str>) -> [f32; 4] {
    let Some(tier) = tier else {
        return [0.0; 4];
    };
    let tier_index = match normalize_defense_tier(tier).as_str() {
        "NONE" => 0.0,
        "VERY LOW" => 1.0,
        "LOW" => 2.0,
        "MEDIUM" => 3.0,
        "HIGH" => 4.0,
        "VERY HIGH" => 5.0,
        _ => return [0.0; 4],
    };
    [4.0, tier_index, 0.0, 0.0]
}

#[cfg(test)]
mod tests {
    use super::{defense_tier_display, defense_tier_overlay_data};

    #[test]
    fn defense_tiers_normalize_api_values() {
        assert_eq!(
            defense_tier_display("VERY_HIGH"),
            ("VERY HIGH".into(), "#aa0000")
        );
        assert_eq!(
            defense_tier_display("very-low"),
            ("VERY LOW".into(), "#00aa00")
        );
        assert_eq!(
            defense_tier_overlay_data(Some("medium")),
            [4.0, 3.0, 0.0, 0.0]
        );
    }
}
