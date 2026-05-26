pub(crate) const DEFENSE_TIERS: &[(&str, &str)] = &[
    ("Very Low", "#00aa00"),
    ("Low", "#55ff55"),
    ("Medium", "#ffff55"),
    ("High", "#ff5555"),
    ("Very High", "#aa0000"),
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
        "NONE" => ("None".to_string(), "#ffffff"),
        "VERY LOW" => ("Very Low".to_string(), "#00aa00"),
        "LOW" => ("Low".to_string(), "#55ff55"),
        "MEDIUM" => ("Medium".to_string(), "#ffff55"),
        "HIGH" => ("High".to_string(), "#ff5555"),
        "VERY HIGH" => ("Very High".to_string(), "#aa0000"),
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
            ("Very High".into(), "#aa0000")
        );
        assert_eq!(
            defense_tier_display("very-low"),
            ("Very Low".into(), "#00aa00")
        );
        assert_eq!(
            defense_tier_overlay_data(Some("medium")),
            [4.0, 3.0, 0.0, 0.0]
        );
    }
}
