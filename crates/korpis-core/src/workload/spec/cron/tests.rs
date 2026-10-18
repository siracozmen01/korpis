// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{Schedule, validate};
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Central European time, by its rule: clocks forward at 02:00 on March's last Sunday and back
/// at 03:00 on October's.
fn berlin() -> TimeZone {
    TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
}

fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn next(schedule: &str, after: &str, zone: &TimeZone) -> Timestamp {
    Schedule::parse(schedule).unwrap().next_after(at(after), zone).unwrap()
}

#[test]
fn five_fields_in_range() {
    for ok in ["0 4 * * *", "*/15 * * * mon-fri", "0 0 1 jan,jul *", "5,35 8-18/2 * * 0"] {
        assert!(validate(ok).is_ok(), "{ok}");
    }
    for bad in ["", "* * * *", "60 * * * *", "* 24 * * *", "*/0 * * * *", "5-1 * * * *", "@daily"] {
        assert!(validate(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_schedule_naming_no_day_that_comes_is_refused() {
    for never in ["0 0 31 4 *", "0 0 30 2 *", "0 0 31 feb,apr,jun *"] {
        assert!(validate(never).is_err(), "{never}");
    }
    for some in ["0 0 29 2 *", "0 0 31 4 mon", "0 0 31 * *"] {
        assert!(validate(some).is_ok(), "{some}");
    }
}

#[test]
fn three_in_the_morning_is_local_in_summer_and_winter() {
    let zone = berlin();
    assert_eq!(next("0 3 * * *", "2026-01-10T00:00:00Z", &zone), at("2026-01-10T02:00:00Z"));
    assert_eq!(next("0 3 * * *", "2026-07-10T00:00:00Z", &zone), at("2026-07-10T01:00:00Z"));
    assert_eq!(next("0 3 * * *", "2026-07-10T01:00:00Z", &zone), at("2026-07-11T01:00:00Z"));
}

#[test]
fn a_time_the_clocks_skip_is_due_when_the_gap_ends_and_a_repeated_one_once() {
    let zone = berlin();
    // 29 March 2026: 02:30 does not exist; the gap ends at 03:00 summer time, 01:00 UTC.
    assert_eq!(next("30 2 * * *", "2026-03-28T12:00:00Z", &zone), at("2026-03-29T01:00:00Z"));
    assert_eq!(next("30 2 * * *", "2026-03-29T01:00:00Z", &zone), at("2026-03-30T00:30:00Z"));
    // Every quarter hour inside the gap is due once, at its end.
    assert_eq!(next("*/15 2 * * *", "2026-03-29T01:00:00Z", &zone), at("2026-03-30T00:00:00Z"));
    // 25 October 2026: 02:30 happens twice; it is due at the first.
    assert_eq!(next("30 2 * * *", "2026-10-24T12:00:00Z", &zone), at("2026-10-25T00:30:00Z"));
    assert_eq!(next("30 2 * * *", "2026-10-25T00:30:00Z", &zone), at("2026-10-26T01:30:00Z"));
}

#[test]
fn days_of_the_month_and_week_both_restricted_match_either() {
    let utc = TimeZone::UTC;
    // 1 January 2026 is a Thursday.
    assert_eq!(next("0 0 13 * fri", "2026-01-01T00:00:00Z", &utc), at("2026-01-02T00:00:00Z"));
    assert_eq!(next("0 0 * * 7", "2026-01-01T00:00:00Z", &utc), at("2026-01-04T00:00:00Z"));
    // A field beginning with `*` is not a restriction of its own, so these are odd days that
    // are Fridays, as in Vixie cron.
    assert_eq!(next("0 0 */2 * fri", "2026-01-01T12:00:00Z", &utc), at("2026-01-09T00:00:00Z"));
    assert_eq!(next("0 0 */2 * fri", "2026-01-09T00:00:00Z", &utc), at("2026-01-23T00:00:00Z"));
    assert_eq!(next("5/20 * * * *", "2026-01-01T00:06:00Z", &utc), at("2026-01-01T00:25:00Z"));
}
