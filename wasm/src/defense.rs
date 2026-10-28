//! Territory defense as the map shows it. The palette belongs to [`DefenseRating`], which the
//! tower calculator's rating uses too, so a rating reads the same colour everywhere.

use sequoia_shared::tower::DefenseRating;

/// What a reported tier is shown as: a rating, a territory without defenses, or a spelling the
/// map does not know, kept as reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefenseTier {
    Rated(DefenseRating),
    None,
    Unknown(String),
}

impl DefenseTier {
    pub fn parse(tier: &str) -> Self {
        if let Some(rating) = DefenseRating::from_api(tier) {
            Self::Rated(rating)
        } else if tier.trim().eq_ignore_ascii_case("none") {
            Self::None
        } else {
            Self::Unknown(tier.to_string())
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Rated(rating) => rating.label(),
            Self::None => "None",
            Self::Unknown(tier) => tier,
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            Self::Rated(rating) => rating.color_hex(),
            Self::None => "#ffffff",
            Self::Unknown(_) => "#e2e0d8",
        }
    }

    /// The overlay's per-territory data: a mode marker and the tier's slot in the shader's
    /// colour table, which mirrors [`DefenseRating::color_hex`].
    pub fn overlay_data(&self) -> [f32; 4] {
        let slot = match self {
            Self::None => 0.0,
            Self::Rated(rating) => match rating {
                DefenseRating::VeryLow => 1.0,
                DefenseRating::Low => 2.0,
                DefenseRating::Medium => 3.0,
                DefenseRating::High => 4.0,
                DefenseRating::VeryHigh => 5.0,
            },
            Self::Unknown(_) => return [0.0; 4],
        };
        [4.0, slot, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_tiers_take_the_rating_palette() {
        let tier = DefenseTier::parse("VERY_HIGH");
        assert_eq!((tier.label(), tier.color()), ("Very High", "#aa0000"));
        assert_eq!(DefenseTier::parse("very-low").color(), "#00aa00");
        assert_eq!(DefenseTier::parse("NONE").label(), "None");
        assert_eq!(DefenseTier::parse("Fortified").label(), "Fortified");
    }

    #[test]
    fn the_overlay_slots_follow_the_ratings() {
        assert_eq!(
            DefenseTier::parse("medium").overlay_data(),
            [4.0, 3.0, 0.0, 0.0]
        );
        assert_eq!(
            DefenseTier::parse("none").overlay_data(),
            [4.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(DefenseTier::parse("?").overlay_data(), [0.0; 4]);
    }

    #[test]
    fn the_calculator_and_a_reported_tier_share_a_colour() {
        for rating in DefenseRating::ALL {
            assert_eq!(
                DefenseTier::parse(rating.label()).color(),
                rating.color_hex()
            );
        }
    }
}
