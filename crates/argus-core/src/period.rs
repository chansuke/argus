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
pub enum BucketWidth {
    M15,
    H1,
    H4,
}

impl BucketWidth {
    pub fn millis(self) -> i64 {
        match self {
            BucketWidth::M15 => 15 * 60 * 1000,
            BucketWidth::H1 => 60 * 60 * 1000,
            BucketWidth::H4 => 4 * 60 * 60 * 1000,
        }
    }
}

/// A time window, aligned to its width in UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Window {
    pub width: BucketWidth,
    pub start: UnixMillis,
}

impl Window {
    /// The window of `width` that holds `at`.
    pub fn containing(width: BucketWidth, at: UnixMillis) -> Self {
        let w = width.millis();
        Window {
            width,
            start: UnixMillis(at.0.div_euclid(w) * w),
        }
    }
    pub fn end(&self) -> UnixMillis {
        UnixMillis(self.start.0 + self.width.millis())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HourBucket(pub i64);

impl HourBucket {
    pub fn of(t: UnixMillis) -> Self {
        HourBucket(t.0.div_euclid(3_600_000))
    }
}

impl Period {
    /// `2026-11`, `2026-11-17`, `2026-11-01..2026-11-15` (both days included), `today`,
    /// `this-week` (from Monday), `last-<n>h`, `last-<n>d`. Calendar forms are read in
    /// `tz` and cover whole days; `last-*` ends at `now`, including its millisecond.
    pub fn parse(s: &str, now: UnixMillis, tz: UtcOffset) -> Option<Self> {
        let offset = i64::from(tz.minutes) * MINUTE;
        let days = |from: i64, n: i64| Period {
            from: UnixMillis(from * DAY - offset),
            to: UnixMillis((from + n) * DAY - offset),
        };
        let today = (now.0 + offset).div_euclid(DAY);
        if let Some((a, b)) = s.split_once("..") {
            let (a, b) = (date(a)?, date(b)?);
            return (a <= b).then(|| days(a, b - a + 1));
        }
        if let Some(n) = s.strip_prefix("last-") {
            let (n, unit) = match (n.strip_suffix('h'), n.strip_suffix('d')) {
                (Some(n), _) => (n, HOUR),
                (_, Some(n)) => (n, DAY),
                _ => return None,
            };
            let n: i64 = digits(n)?;
            let span = n.checked_mul(unit).filter(|_| n > 0)?;
            // Half-open: what happened in the millisecond of `now` is already past.
            let to = now.0.checked_add(1)?;
            return Some(Period {
                from: UnixMillis(to.checked_sub(span)?),
                to: UnixMillis(to),
            });
        }
        match s {
            "today" => Some(days(today, 1)),
            // Day 0 (1970-01-01) was a Thursday: Monday is 3 days before it.
            "this-week" => Some(days(today - (today + 3).rem_euclid(7), 7)),
            _ => match date(s) {
                Some(day) => Some(days(day, 1)),
                None => {
                    let (y, m) = s.split_once('-')?;
                    let (y, m) = (year(y)?, two(m)?);
                    let first = civil_days(y, m, 1)?;
                    Some(days(first, month_len(y, m)))
                }
            },
        }
    }
    pub fn contains(&self, t: UnixMillis) -> bool {
        self.from <= t && t < self.to
    }
    /// Buckets fully inside the period, plus whether the ends are hour-aligned (if not,
    /// the caller replays the edge hours for exact numbers).
    pub fn hours(&self) -> (std::ops::Range<i64>, bool) {
        let start = self.from.0.div_euclid(HOUR) + i64::from(self.from.0.rem_euclid(HOUR) != 0);
        let end = self.to.0.div_euclid(HOUR).max(start);
        let aligned = self.from.0.rem_euclid(HOUR) == 0 && self.to.0.rem_euclid(HOUR) == 0;
        (start..end, aligned)
    }
}

impl UtcOffset {
    /// `YYYY-MM-DD` of the local day `t` falls in.
    pub fn date(self, t: UnixMillis) -> String {
        let day = (t.0 + i64::from(self.minutes) * MINUTE).div_euclid(DAY);
        let (y, m, d) = civil_from_days(day);
        format!("{y:04}-{m:02}-{d:02}")
    }
}

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// ASCII digits only (no sign, no spaces), as `str::parse` alone would also accept `+7`.
fn digits(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn year(s: &str) -> Option<i64> {
    (s.len() == 4).then(|| digits(s)).flatten()
}

fn two(s: &str) -> Option<i64> {
    (s.len() == 2).then(|| digits(s)).flatten()
}

/// `YYYY-MM-DD` as days since 1970-01-01.
fn date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (y, m, d) = (year(y)?, two(m)?, two(d)?);
    (1..=month_len(y, m)).contains(&d).then_some(())?;
    civil_days(y, m, d)
}

fn month_len(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). `None` for a month outside 1..=12.
fn civil_days(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of `civil_days` (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
