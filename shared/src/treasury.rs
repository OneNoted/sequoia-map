/// Treasury bonus level — derived purely from how long a guild has held a territory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreasuryLevel {
    VeryLow,
    Low,
    Medium,
    High,
    VeryHigh,
}

impl TreasuryLevel {
    /// Determine treasury level from hold duration in seconds.
    pub fn from_held_seconds(secs: i64) -> Self {
        const HOUR: i64 = 3600;
        const DAY: i64 = 86400;
        if secs >= 12 * DAY {
            Self::VeryHigh
        } else if secs >= 5 * DAY {
            Self::High
        } else if secs >= DAY {
            Self::Medium
        } else if secs >= HOUR {
            Self::Low
        } else {
            Self::VeryLow
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::VeryLow => "Very Low",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::VeryHigh => "Very High",
        }
    }

    /// Minecraft-style color as RGB bytes.
    pub fn color_rgb(self) -> (u8, u8, u8) {
        match self {
            Self::VeryLow => (0, 170, 0),
            Self::Low => (85, 255, 85),
            Self::Medium => (255, 255, 85),
            Self::High => (85, 255, 85),
            Self::VeryHigh => (85, 255, 255),
        }
    }

    /// Normalized RGB for GPU shaders (0.0..1.0).
    pub fn color_f32(self) -> [f32; 3] {
        let (r, g, b) = self.color_rgb();
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
    }

    /// Resource production buff percentage.
    pub fn buff_percent(self) -> u8 {
        match self {
            Self::VeryLow => 0,
            Self::Low => 10,
            Self::Medium => 20,
            Self::High => 25,
            Self::VeryHigh => 30,
        }
    }
}
