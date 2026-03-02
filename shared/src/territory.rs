use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ingest::TerritoryRuntimeData;

pub type TerritoryMap = HashMap<String, Territory>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Territory {
    pub guild: GuildRef,
    pub acquired: DateTime<Utc>,
    pub location: Region,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub connections: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<TerritoryRuntimeData>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Resources {
    #[serde(default)]
    pub emeralds: i32,
    #[serde(default)]
    pub ore: i32,
    #[serde(default)]
    pub crops: i32,
    #[serde(default)]
    pub fish: i32,
    #[serde(default)]
    pub wood: i32,
}

impl Resources {
    pub fn has_emeralds(&self) -> bool {
        self.emeralds > 9000
    }

    pub fn has_double_emeralds(&self) -> bool {
        self.emeralds >= 18000
    }

    pub fn has_double_ore(&self) -> bool {
        self.ore >= 7200
    }

    pub fn has_double_crops(&self) -> bool {
        self.crops >= 7200
    }

    pub fn has_double_fish(&self) -> bool {
        self.fish >= 7200
    }

    pub fn has_double_wood(&self) -> bool {
        self.wood >= 7200
    }

    pub fn is_empty(&self) -> bool {
        self.emeralds == 0 && self.ore == 0 && self.crops == 0 && self.fish == 0 && self.wood == 0
    }

    pub fn has_all(&self) -> bool {
        self.emeralds > 0 && self.ore > 0 && self.crops > 0 && self.fish > 0 && self.wood > 0
    }

    /// Encode notable resources into `[mode, idx_a, idx_b, flags]` for GPU/canvas.
    ///
    /// Resource indices: 1=ore, 2=crops, 3=fish, 4=wood (emerald excluded).
    /// Double emeralds are encoded as bit 10 in flags → green checker overlay.
    /// - mode 0: no notable resources (guild color, possibly with green checker)
    /// - mode 1: solid single resource fill
    /// - mode 2: diagonal split (two resources)
    /// - mode 3: multi-stripe (3+ resources, bitmask encoded)
    pub fn highlight_data(&self) -> [f32; 4] {
        let dbl_em = self.has_double_emeralds();
        let dbl_em_bit = if dbl_em { 1u32 << 10 } else { 0 };

        // Collect non-emerald resources only.
        let mut notable: [(u8, bool); 4] = [(0, false); 4]; // (index, is_double)
        let mut count = 0usize;

        if self.ore > 0 {
            notable[count] = (1, self.has_double_ore());
            count += 1;
        }
        if self.crops > 0 {
            notable[count] = (2, self.has_double_crops());
            count += 1;
        }
        if self.fish > 0 {
            notable[count] = (3, self.has_double_fish());
            count += 1;
        }
        if self.wood > 0 {
            notable[count] = (4, self.has_double_wood());
            count += 1;
        }

        match count {
            0 => [0.0, 0.0, 0.0, dbl_em_bit as f32],
            1 => {
                let (idx, dbl) = notable[0];
                let flags = (if dbl { 1u32 } else { 0 }) | dbl_em_bit;
                [1.0, idx as f32, 0.0, flags as f32]
            }
            2 => {
                let (idx_a, dbl_a) = notable[0];
                let (idx_b, dbl_b) = notable[1];
                let flags =
                    (if dbl_a { 1u32 } else { 0 }) | (if dbl_b { 2u32 } else { 0 }) | dbl_em_bit;
                [2.0, idx_a as f32, idx_b as f32, flags as f32]
            }
            _ => {
                // mode 3: pack stripe_mask (lower 5 bits) and double_mask (bits 5-9)
                let mut stripe_mask = 0u32;
                let mut double_mask = 0u32;
                for (idx, dbl) in notable.iter().copied().take(count) {
                    stripe_mask |= 1u32 << idx;
                    if dbl {
                        double_mask |= 1u32 << idx;
                    }
                }
                let flags = stripe_mask | (double_mask << 5) | dbl_em_bit;
                [3.0, 0.0, 0.0, flags as f32]
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildRef {
    pub uuid: String,
    pub name: String,
    pub prefix: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<(u8, u8, u8)>,
}

/// Axis-aligned rectangle in world coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub start: [i32; 2],
    pub end: [i32; 2],
}

impl Region {
    pub const fn width(&self) -> i32 {
        (self.end[0] - self.start[0]).abs()
    }

    pub const fn height(&self) -> i32 {
        (self.end[1] - self.start[1]).abs()
    }

    pub const fn midpoint_x(&self) -> i32 {
        (self.start[0] + self.end[0]) / 2
    }

    pub const fn midpoint_y(&self) -> i32 {
        (self.start[1] + self.end[1]) / 2
    }

    pub const fn left(&self) -> i32 {
        if self.start[0] < self.end[0] {
            self.start[0]
        } else {
            self.end[0]
        }
    }

    pub const fn right(&self) -> i32 {
        if self.start[0] > self.end[0] {
            self.start[0]
        } else {
            self.end[0]
        }
    }

    pub const fn top(&self) -> i32 {
        if self.start[1] < self.end[1] {
            self.start[1]
        } else {
            self.end[1]
        }
    }

    pub const fn bottom(&self) -> i32 {
        if self.start[1] > self.end[1] {
            self.start[1]
        } else {
            self.end[1]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Resources;

    #[test]
    fn highlight_data_mode0_no_resources() {
        let resources = Resources::default();
        assert_eq!(resources.highlight_data(), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn highlight_data_mode0_only_double_emeralds() {
        let resources = Resources {
            emeralds: 18_000,
            ..Resources::default()
        };
        assert_eq!(resources.highlight_data(), [0.0, 0.0, 0.0, 1024.0]);
    }

    #[test]
    fn highlight_data_mode1_single_resource() {
        let resources = Resources {
            ore: 1,
            ..Resources::default()
        };
        assert_eq!(resources.highlight_data(), [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn highlight_data_mode1_single_double_resource() {
        let resources = Resources {
            ore: 7_200,
            ..Resources::default()
        };
        assert_eq!(resources.highlight_data(), [1.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn highlight_data_mode2_two_resources_with_doubles() {
        let resources = Resources {
            ore: 7_200,
            fish: 7_200,
            ..Resources::default()
        };
        assert_eq!(resources.highlight_data(), [2.0, 1.0, 3.0, 3.0]);
    }

    #[test]
    fn highlight_data_mode3_three_plus_resources_stripe_mask() {
        let resources = Resources {
            emeralds: 18_000,
            ore: 7_200,
            crops: 100,
            wood: 7_200,
            ..Resources::default()
        };
        assert_eq!(resources.highlight_data(), [3.0, 0.0, 0.0, 1_622.0]);
    }
}
