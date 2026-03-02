use serde::{Deserialize, Serialize};

pub const BASE_HOURLY_SR: f64 = 120.0;

/// Regression multiplier per territory slot (1-indexed in game terms).
/// Slot 23+ is clamped to the final value (0.20).
pub const REGRESSION_MULTIPLIERS: [f64; 22] = [
    3.00, 2.00, 1.00, 0.90, 0.80, 0.75, 0.75, 0.70, 0.70, 0.65, 0.65, 0.60, 0.60, 0.55, 0.55, 0.50,
    0.45, 0.40, 0.35, 0.30, 0.25, 0.20,
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeasonScalarSample {
    pub sampled_at: String,
    pub season_id: i32,
    pub scalar_weighted: f64,
    pub scalar_raw: f64,
    pub confidence: f64,
    pub sample_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SeasonScalarCurrent {
    pub sample: Option<SeasonScalarSample>,
}

fn regression_multiplier(idx: usize) -> f64 {
    REGRESSION_MULTIPLIERS
        .get(idx)
        .copied()
        .unwrap_or(REGRESSION_MULTIPLIERS[REGRESSION_MULTIPLIERS.len() - 1])
}

/// Sum of regression multipliers for `territory_count` owned territories.
pub fn weighted_units(territory_count: usize) -> f64 {
    (0..territory_count).map(regression_multiplier).sum()
}

/// Passive season rating generated per hour from holding territories.
pub fn passive_sr_per_hour(territory_count: usize, scalar: f64) -> f64 {
    BASE_HOURLY_SR * scalar * weighted_units(territory_count)
}

/// Passive season rating generated every 5 seconds from holding territories.
pub fn passive_sr_per_5s(territory_count: usize, scalar: f64) -> f64 {
    passive_sr_per_hour(territory_count, scalar) / 720.0
}

#[cfg(test)]
mod tests {
    use super::{passive_sr_per_5s, passive_sr_per_hour, weighted_units};

    fn assert_close(actual: f64, expected: f64) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-9,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn weighted_units_for_five_territories_includes_fifth_penalty() {
        // 3.00 + 2.00 + 1.00 + 0.90 + 0.80 = 7.70
        assert_close(weighted_units(5), 7.7);
    }

    #[test]
    fn weighted_units_clamps_to_twenty_two_plus_penalty() {
        let units_22 = weighted_units(22);
        let units_23 = weighted_units(23);
        assert_close(units_23 - units_22, 0.2);
    }

    #[test]
    fn passive_hour_and_five_second_outputs_are_consistent() {
        let hourly = passive_sr_per_hour(9, 2.5);
        let per_5s = passive_sr_per_5s(9, 2.5);
        assert_close(per_5s * 720.0, hourly);
    }
}
