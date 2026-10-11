//! Heat-map colouring by how often each territory changed hands.

use crate::colors::heat_color_for_intensity;

/// Where each capture count ranks among the territories on the map, for colouring.
///
/// A count's intensity is the share of the ranked territories taken that often or less (its
/// empirical CDF), so one territory far ahead of the rest cannot push everyone else into the
/// lowest colour, and the palette is spread evenly however skewed the counts are. Equal counts
/// share a colour and more captures never colour cooler. Territories with no captures are not
/// ranked and keep the lowest colour, as before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeatScale {
    /// The ranked (positive) counts, ascending.
    sorted: Vec<u64>,
}

impl HeatScale {
    /// Ranks the positive `counts`; zeros are left out.
    pub fn new(counts: impl IntoIterator<Item = u64>) -> Self {
        let mut sorted: Vec<u64> = counts.into_iter().filter(|&count| count > 0).collect();
        sorted.sort_unstable();
        Self { sorted }
    }

    /// How many territories are ranked.
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// From 0 (no captures) to 1 (the most captured).
    pub fn intensity(&self, count: u64) -> f64 {
        if count == 0 || self.sorted.is_empty() {
            return 0.0;
        }
        let at_or_below = self.sorted.partition_point(|&ranked| ranked <= count);
        at_or_below as f64 / self.sorted.len() as f64
    }

    pub fn color(&self, count: u64) -> (u8, u8, u8) {
        heat_color_for_intensity(self.intensity(count))
    }

    /// The smallest ranked count with at least `share` of the ranked territories at or below
    /// it, which is where that intensity begins: the count to label a legend stop with.
    /// `None` when nothing is ranked.
    pub fn count_at(&self, share: f64) -> Option<u64> {
        let len = self.sorted.len();
        if len == 0 {
            return None;
        }
        let rank = (share.clamp(0.0, 1.0) * len as f64).ceil() as usize;
        Some(self.sorted[rank.clamp(1, len) - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::HeatScale;
    use crate::colors::heat_color_for_intensity;

    /// Eight territories at each ordinary level and one taken about four times as often as
    /// the busiest of them.
    fn with_outlier() -> HeatScale {
        let ordinary = [10, 20, 30, 40, 50]
            .into_iter()
            .flat_map(|level| [level; 8]);
        HeatScale::new(ordinary.chain([200]))
    }

    #[test]
    fn an_outlier_leaves_ordinary_levels_spread_across_the_palette() {
        let scale = with_outlier();
        let levels = [10, 20, 30, 40, 50].map(|level| scale.intensity(level));
        // The quietest ordinary level stays in the cool fifth, the busiest in the hot fifth,
        // and neighbouring levels are at least a band apart.
        assert!(levels[0] < 0.2 + f64::EPSILON, "{levels:?}");
        assert!(levels[4] > 0.8, "{levels:?}");
        for pair in levels.windows(2) {
            assert!(pair[1] - pair[0] >= 0.15, "{levels:?}");
        }
    }

    #[test]
    fn a_bigger_outlier_does_not_recolour_anyone_else() {
        let ordinary = [10, 20, 30, 40, 50]
            .into_iter()
            .flat_map(|level| [level; 8]);
        let modest = HeatScale::new(ordinary.clone().chain([60]));
        let extreme = HeatScale::new(ordinary.chain([1_000_000]));
        for level in [10, 20, 30, 40, 50] {
            assert_eq!(modest.color(level), extreme.color(level), "level {level}");
        }
        assert_eq!(extreme.intensity(1_000_000), 1.0);
    }

    #[test]
    fn more_captures_never_colour_cooler_and_ties_share_a_colour() {
        let scale = HeatScale::new([7, 3, 3, 12, 7, 7, 1, 90, 12, 3]);
        let mut previous = scale.intensity(0);
        for count in 0..=100 {
            let intensity = scale.intensity(count);
            assert!(intensity >= previous, "count {count}");
            previous = intensity;
        }
        assert!(scale.intensity(3) < scale.intensity(7));
        assert!(scale.intensity(12) < scale.intensity(90));
        assert_eq!(scale.color(7), scale.color(7));
        // Every territory at 7 is ranked together, above all at 3.
        assert_eq!(scale.intensity(7), 0.7);
    }

    #[test]
    fn no_captures_keeps_the_lowest_colour() {
        let scale = HeatScale::new([0, 0, 5, 9]);
        assert_eq!(scale.len(), 2);
        assert_eq!(scale.intensity(0), 0.0);
        assert_eq!(scale.color(0), heat_color_for_intensity(0.0));
        assert!(scale.intensity(5) > 0.0);
    }

    #[test]
    fn empty_and_all_zero_data_colour_everything_lowest() {
        for scale in [HeatScale::new([]), HeatScale::new([0, 0, 0])] {
            assert!(scale.is_empty());
            assert_eq!(scale.intensity(0), 0.0);
            assert_eq!(scale.intensity(42), 0.0);
            assert_eq!(scale.count_at(0.5), None);
        }
    }

    #[test]
    fn a_single_or_uniform_count_is_the_hottest() {
        assert_eq!(HeatScale::new([4]).intensity(4), 1.0);
        let uniform = HeatScale::new([6; 25]);
        assert_eq!(uniform.intensity(6), 1.0);
        assert_eq!(uniform.count_at(0.0), Some(6));
        assert_eq!(uniform.count_at(1.0), Some(6));
    }

    #[test]
    fn extreme_counts_rank_without_overflow() {
        let scale = HeatScale::new([1, u64::MAX - 1, u64::MAX]);
        assert_eq!(scale.intensity(u64::MAX), 1.0);
        assert!(scale.intensity(u64::MAX - 1) < 1.0);
        assert!(scale.intensity(1) > 0.0);
        assert_eq!(scale.count_at(1.0), Some(u64::MAX));
    }

    #[test]
    fn legend_stops_name_the_count_where_each_colour_begins() {
        let scale = with_outlier();
        assert_eq!(scale.count_at(0.0), Some(10));
        assert_eq!(scale.count_at(1.0), Some(200));
        for share in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let count = scale.count_at(share).unwrap();
            assert!(scale.intensity(count) >= share, "share {share}");
            // Nothing below that count reaches the share.
            assert!(
                scale.intensity(count - 1) < share.max(f64::MIN_POSITIVE),
                "share {share}"
            );
        }
    }
}
