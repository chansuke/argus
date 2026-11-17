//! Time-period slicing for analytics.

use crate::ids::UnixMillis;

/// Half-open `[from, to)`, UTC instants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Period {
    pub from: UnixMillis,
    pub to: UnixMillis,
}

/// Fixed offset used to interpret calendar input (`2026-11-17` means that day in JST).
/// std has no tz database; a fixed offset is enough for a single local user.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UtcOffset {
    pub minutes: i16,
}

/// Hours since the Unix epoch. Analytics aggregate per hour, so any whole-hour period in
/// any whole-hour offset is answered exactly from aggregates without replaying logs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HourBucket(pub i64);

impl HourBucket {
    pub fn of(t: UnixMillis) -> Self {
        HourBucket(t.0.div_euclid(3_600_000))
    }
}

impl Period {
    /// `2026-11`, `2026-11-17`, `2026-11-01..2026-11-15`, `today`, `this-week`,
    /// `last-24h`, `last-7d`.
    pub fn parse(s: &str, now: UnixMillis, tz: UtcOffset) -> Option<Self> {
        todo!("not implemented")
    }
    pub fn contains(&self, t: UnixMillis) -> bool {
        self.from <= t && t < self.to
    }
    /// Buckets fully inside the period, plus whether the ends are hour-aligned (if not,
    /// the caller replays the edge hours for exact numbers).
    pub fn hours(&self) -> (std::ops::Range<i64>, bool) {
        todo!("not implemented")
    }
}
