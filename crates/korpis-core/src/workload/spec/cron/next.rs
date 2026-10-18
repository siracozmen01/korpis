// SPDX-License-Identifier: AGPL-3.0-or-later

//! When a schedule is next due, read in a time zone (§4.1 of docs/05-scheduling.md). The local
//! times the schedule names are walked in order and each is made an instant in the zone: a local
//! time a daylight-saving change skips becomes the first instant after the gap, and one it
//! repeats becomes its first occurrence alone, so each is due once and in order.

use super::Schedule;
use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};

/// How many days ahead a schedule is searched: past any leap day, a century's skipped one
/// included.
const HORIZON_DAYS: u32 = 8 * 366;

impl Schedule {
    /// The first instant after `after` at which the schedule is due, read in `zone`; `None` only
    /// for a schedule no day of the next eight years matches, which validation refuses.
    #[must_use]
    pub fn next_after(&self, after: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
        let mut date = zone.to_datetime(after).date();
        for _ in 0..HORIZON_DAYS {
            let found = self.due_on(date).then(|| self.first_on(date, after, zone)).flatten();
            if found.is_some() {
                return found;
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    fn due_on(&self, date: Date) -> bool {
        let in_month = self.days & 1 << date.day() != 0;
        let on_weekday = self.weekdays & 1 << date.weekday().to_sunday_zero_offset() != 0;
        let day = if self.any_day || self.any_weekday {
            in_month && on_weekday
        } else {
            in_month || on_weekday
        };
        day && self.months & 1 << date.month() != 0
    }

    /// The first time due on `date` that is after `after`.
    fn first_on(&self, date: Date, after: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
        let hours = (0..24i8).filter(|h| self.hours & 1 << h != 0);
        hours
            .flat_map(|h| (0..60i8).filter(|m| self.minutes & 1 << m != 0).map(move |m| (h, m)))
            .filter_map(|(h, m)| instant(zone, date.at(h, m, 0, 0)))
            .find(|at| *at > after)
    }
}

/// `local` in `zone`: in a gap, the instant the gap ends; in a fold, the first of the two.
fn instant(zone: &TimeZone, local: DateTime) -> Option<Timestamp> {
    match zone.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { offset } | AmbiguousOffset::Fold { before: offset, .. } => {
            offset.to_timestamp(local).ok()
        }
        AmbiguousOffset::Gap { before, .. } => {
            let past = before.to_timestamp(local).ok()?;
            zone.preceding(past).next().map(|t| t.timestamp())
        }
    }
}
