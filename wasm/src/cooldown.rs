//! A new capture's cooldown, on the map's timer clock: now, or the History time.

/// How long a captured territory stays on cooldown.
pub const COOLDOWN_SECS: i64 = 600;

/// The cooldown fill pulses over this many final seconds.
pub const PULSE_SECS: i64 = 10;

/// When each territory's cooldown ends, sorted, so a frame can tell whether any is in its
/// final [`PULSE_SECS`] without scanning the territories.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CooldownEnds(Vec<i64>);

impl CooldownEnds {
    /// From each territory's capture time, in clock seconds.
    pub fn new(acquired: impl IntoIterator<Item = i64>) -> Self {
        let mut ends: Vec<i64> = acquired
            .into_iter()
            .map(|at| at.saturating_add(COOLDOWN_SECS))
            .collect();
        ends.sort_unstable();
        Self(ends)
    }

    /// Whether some cooldown is in its final [`PULSE_SECS`] at `clock_secs`. A capture
    /// later than the clock is still at the start of its cooldown.
    pub fn pulsing_at(&self, clock_secs: i64) -> bool {
        let next = self.0.partition_point(|&end| end <= clock_secs);
        self.0
            .get(next)
            .is_some_and(|&end| end - clock_secs <= PULSE_SECS)
    }
}

/// [`CooldownEnds`] kept for one territory revision. Every frame that could pulse asks for
/// them; they are rebuilt only when the territories, or whether their cooldowns show at
/// all, change.
#[derive(Debug, Default)]
pub struct CooldownEndsCache {
    key: Option<(u64, bool)>,
    ends: CooldownEnds,
}

impl CooldownEndsCache {
    /// The ends for territories at `revision`; `acquired` lists their capture times and is
    /// called only when the revision or `shown` changed since the last call.
    pub fn get<I>(
        &mut self,
        revision: u64,
        shown: bool,
        acquired: impl FnOnce() -> I,
    ) -> &CooldownEnds
    where
        I: IntoIterator<Item = i64>,
    {
        if self.key != Some((revision, shown)) {
            self.ends = if shown {
                CooldownEnds::new(acquired())
            } else {
                CooldownEnds::default()
            };
            self.key = Some((revision, shown));
        }
        &self.ends
    }
}

#[cfg(test)]
mod tests {
    use super::{COOLDOWN_SECS, CooldownEnds, CooldownEndsCache, PULSE_SECS};

    #[test]
    fn pulses_only_in_the_final_seconds_of_a_cooldown() {
        let ends = CooldownEnds::new([1_000]);
        let end = 1_000 + COOLDOWN_SECS;
        assert!(!ends.pulsing_at(1_000));
        assert!(!ends.pulsing_at(end - PULSE_SECS - 1));
        assert!(ends.pulsing_at(end - PULSE_SECS));
        assert!(ends.pulsing_at(end - 1));
        assert!(!ends.pulsing_at(end));
        assert!(!ends.pulsing_at(end + 30));
    }

    #[test]
    fn any_cooldown_in_its_final_seconds_pulses() {
        let ends = CooldownEnds::new([5_000, 1_000, 3_000]);
        assert!(ends.pulsing_at(3_000 + COOLDOWN_SECS - 3));
        assert!(!ends.pulsing_at(3_000 + COOLDOWN_SECS));
        assert!(ends.pulsing_at(5_000 + COOLDOWN_SECS - 1));
    }

    #[test]
    fn the_cache_rebuilds_only_for_new_territories_or_visibility() {
        let mut cache = CooldownEndsCache::default();
        let builds = std::cell::Cell::new(0);
        let mut get = |revision, shown| {
            cache
                .get(revision, shown, || {
                    builds.set(builds.get() + 1);
                    [1_000]
                })
                .pulsing_at(1_000 + COOLDOWN_SECS - 1)
        };
        assert!(get(7, true));
        // The same revision, as on every frame between territory updates: no rebuild.
        for _ in 0..50 {
            assert!(get(7, true));
        }
        assert!(!get(7, false));
        assert!(get(8, true));
        assert_eq!(builds.get(), 2);
    }

    #[test]
    fn future_missing_and_absent_captures_do_not_pulse() {
        assert!(!CooldownEnds::default().pulsing_at(0));
        assert!(!CooldownEnds::new([2_000]).pulsing_at(1_000));
        // A missing capture time reads as the Unix epoch: long expired.
        assert!(!CooldownEnds::new([0]).pulsing_at(1_793_000_000));
    }
}
