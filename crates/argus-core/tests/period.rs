use argus_core::ids::UnixMillis;
use argus_core::period::{Period, UtcOffset};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const JST: UtcOffset = UtcOffset { minutes: 540 };

/// 2026-11-17T00:00:00Z.
const NOV_17_UTC: i64 = 1_794_873_600_000;

fn period(from: i64, to: i64) -> Period {
    Period {
        from: UnixMillis(from),
        to: UnixMillis(to),
    }
}

fn parse(s: &str, now: i64) -> Option<Period> {
    Period::parse(s, UnixMillis(now), JST)
}

#[test]
fn a_calendar_day_or_month_is_that_span_in_the_local_offset() {
    let local_midnight = NOV_17_UTC - 9 * HOUR;
    assert_eq!(
        parse("2026-11-17", 0),
        Some(period(local_midnight, local_midnight + DAY))
    );
    let nov_1 = local_midnight - 16 * DAY;
    assert_eq!(parse("2026-11", 0), Some(period(nov_1, nov_1 + 30 * DAY)));
    let dec_1 = nov_1 + 30 * DAY;
    assert_eq!(parse("2026-12", 0), Some(period(dec_1, dec_1 + 31 * DAY)));
    let feb_1 = Period::parse("2028-02", UnixMillis(0), UtcOffset { minutes: 0 }).unwrap();
    assert_eq!(feb_1.to.0 - feb_1.from.0, 29 * DAY, "2028 is a leap year");
}

#[test]
fn a_date_range_includes_its_last_day() {
    let local_midnight = NOV_17_UTC - 9 * HOUR;
    let nov_1 = local_midnight - 16 * DAY;
    assert_eq!(
        parse("2026-11-01..2026-11-15", 0),
        Some(period(nov_1, nov_1 + 15 * DAY))
    );
    assert_eq!(parse("2026-11-15..2026-11-01", 0), None);
}

#[test]
fn relative_periods_are_measured_from_now() {
    let now = NOV_17_UTC + 5 * HOUR + 123; // 14:00 JST on Tuesday 2026-11-17.
    let local_midnight = NOV_17_UTC - 9 * HOUR;
    assert_eq!(
        parse("today", now),
        Some(period(local_midnight, local_midnight + DAY))
    );
    let monday = local_midnight - DAY;
    assert_eq!(
        parse("this-week", now),
        Some(period(monday, monday + 7 * DAY))
    );
    assert_eq!(parse("last-24h", now), Some(period(now - DAY, now)));
    assert_eq!(parse("last-7d", now), Some(period(now - 7 * DAY, now)));
    assert_eq!(parse("last-3h", now), Some(period(now - 3 * HOUR, now)));
}

#[test]
fn malformed_periods_are_refused() {
    for s in [
        "",
        "2026",
        "2026-13",
        "2026-00",
        "2026-02-30",
        "2026-11-31",
        "2026-1-01",
        "last-0d",
        "last-7w",
        "last-d",
        "last-7時",
        "yesterday",
        "2026-11-01..",
        "2026-11-01..2026-11",
    ] {
        assert_eq!(parse(s, NOV_17_UTC), None, "{s:?}");
    }
}

#[test]
fn hours_are_the_whole_buckets_inside_and_whether_the_ends_align() {
    assert_eq!(period(0, 3 * HOUR).hours(), (0..3, true));
    assert_eq!(period(1, 3 * HOUR).hours(), (1..3, false));
    assert_eq!(period(HOUR, 3 * HOUR - 1).hours(), (1..2, false));
    let (inside, aligned) = period(HOUR + 1, HOUR + 2).hours();
    assert!(inside.is_empty() && !aligned);
    assert_eq!(period(-2 * HOUR, 0).hours(), (-2..0, true));
}
