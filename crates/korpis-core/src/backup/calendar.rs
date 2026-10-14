// SPDX-License-Identifier: AGPL-3.0-or-later

//! The UTC calendar retention buckets backups by: the hour, the day, the ISO week, the month, and
//! the year a moment falls in. Civil dates are computed from days since 1970-01-01 by Howard
//! Hinnant's algorithms, exact for every date a backup can carry.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the epoch; a moment before it counts as the epoch.
fn seconds(at: SystemTime) -> i64 {
    let s = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    i64::try_from(s).unwrap_or(i64::MAX)
}

/// Days since 1970-01-01 of `at`.
fn days(at: SystemTime) -> i64 {
    seconds(at).div_euclid(86_400)
}

/// The civil date (year, month, day) of day `z` since 1970-01-01.
fn civil(z: i64) -> (i64, i64, i64) {
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

/// Days since 1970-01-01 of the civil date (year, month, day).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// The hour `at` falls in.
#[must_use]
pub fn hour(at: SystemTime) -> i64 {
    seconds(at).div_euclid(3600)
}

/// The day `at` falls in.
#[must_use]
pub fn day(at: SystemTime) -> i64 {
    days(at)
}

/// The ISO week `at` falls in, as (ISO year, week): weeks begin on Monday, and a week belongs to
/// the year its Thursday is in.
#[must_use]
pub fn week(at: SystemTime) -> (i64, i64) {
    let d = days(at);
    // 1970-01-01 was a Thursday: Monday is 0.
    let thursday = d - (d + 3).rem_euclid(7) + 3;
    let (year, _, _) = civil(thursday);
    (year, (thursday - days_from_civil(year, 1, 1)) / 7 + 1)
}

/// The month `at` falls in, as (year, month).
#[must_use]
pub fn month(at: SystemTime) -> (i64, i64) {
    let (y, m, _) = civil(days(at));
    (y, m)
}

/// The year `at` falls in.
#[must_use]
pub fn year(at: SystemTime) -> i64 {
    civil(days(at)).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn on(y: i64, m: i64, d: i64, hour: u64) -> SystemTime {
        let days = u64::try_from(days_from_civil(y, m, d)).unwrap();
        UNIX_EPOCH + Duration::from_secs(days * 86_400 + hour * 3600)
    }

    #[test]
    fn civil_dates_round_trip_across_leap_years_and_centuries() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2024, 12, 31), (2100, 3, 1), (2026, 10, 14)]
        {
            assert_eq!(civil(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(month(on(2026, 10, 14, 23)), (2026, 10));
        assert_eq!(year(on(2026, 12, 31, 23)), 2026);
        assert_eq!(day(on(2026, 10, 14, 0)) + 1, day(on(2026, 10, 15, 0)));
        assert_eq!(hour(on(2026, 10, 14, 5)) + 1, hour(on(2026, 10, 14, 6)));
    }

    #[test]
    fn iso_weeks_begin_on_monday_and_belong_to_their_thursdays_year() {
        assert_eq!(week(on(2026, 10, 14, 12)), (2026, 42));
        assert_eq!(week(on(2026, 10, 12, 0)), week(on(2026, 10, 18, 23)), "Monday to Sunday");
        assert_ne!(week(on(2026, 10, 11, 23)), week(on(2026, 10, 12, 0)));
        assert_eq!(week(on(2021, 1, 3, 0)), (2020, 53), "a Sunday in the last week of 2020");
        assert_eq!(week(on(2024, 12, 30, 0)), (2025, 1), "a Monday in the first week of 2025");
        assert_eq!(week(on(2015, 12, 31, 0)), (2015, 53));
    }
}
