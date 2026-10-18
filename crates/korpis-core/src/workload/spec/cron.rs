// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cron schedules: five fields (minute, hour, day of month, month, day of week), each a list of
//! `*`, `n`, or `n-m`, optionally stepped with `/k` (`n/k` runs from `n` to the field's end).
//! Months and days of the week may be named, and Sunday is 0 or 7. As in Vixie cron, when both
//! the day of the month and the day of the week are restricted (neither begins with `*`), a day
//! matching either is due.
//!
//! A schedule is read in a time zone (§4.1 of docs/05-scheduling.md): see [`Schedule::next_after`].

mod next;

use super::SpecError;

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
/// The longest each month can be, February in a leap year.
const LONGEST: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// A parsed schedule: for each field, the set of values it is due at, bit `n` for value `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    /// Whether the day of the month began with `*`.
    any_day: bool,
    /// Whether the day of the week began with `*`.
    any_weekday: bool,
}

fn value(s: &str, lo: u32, hi: u32, names: &[&str]) -> Option<u32> {
    let lowercase = s.to_ascii_lowercase();
    let by_name = names
        .iter()
        .position(|n| *n == lowercase)
        .and_then(|i| u32::try_from(i).ok())
        .map(|i| i + lo);
    let n = by_name.or_else(|| s.parse().ok())?;
    (lo..=hi).contains(&n).then_some(n)
}

/// The values one item of a field names, as a bit set, or `None` if it names none validly.
fn item(s: &str, lo: u32, hi: u32, names: &[&str]) -> Option<u64> {
    let (range, step) = s.split_once('/').map_or((s, None), |(r, k)| (r, Some(k)));
    let step = match step {
        None => 1,
        Some(k) => k.parse::<u32>().ok().filter(|k| *k > 0 && *k <= hi)?,
    };
    let (first, last) = match range.split_once('-') {
        _ if range == "*" => (lo, hi),
        Some((a, b)) => (value(a, lo, hi, names)?, value(b, lo, hi, names)?),
        None if step > 1 => (value(range, lo, hi, names)?, hi),
        None => {
            let n = value(range, lo, hi, names)?;
            (n, n)
        }
    };
    if first > last {
        return None;
    }
    Some((first..=last).step_by(usize::try_from(step).ok()?).fold(0, |bits, n| bits | 1 << n))
}

/// A field's values as a bit set.
fn field(s: &str, lo: u32, hi: u32, names: &[&str]) -> Option<u64> {
    s.split(',').try_fold(0, |bits, i| Some(bits | item(i, lo, hi, names)?))
}

impl Schedule {
    /// Parses `schedule`.
    ///
    /// # Errors
    /// It is not five valid fields.
    pub fn parse(schedule: &str) -> Result<Self, SpecError> {
        let bad = || SpecError::new("schedule", "five cron fields");
        let fields: Vec<&str> = schedule.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else { return Err(bad()) };
        let weekdays = field(weekday, 0, 7, &DAYS).ok_or_else(bad)?;
        // Sunday is 0 or 7.
        let weekdays = (weekdays | weekdays >> 7) & 0x7f;
        Ok(Self {
            minutes: field(minute, 0, 59, &[]).ok_or_else(bad)?,
            hours: u32::try_from(field(hour, 0, 23, &[]).ok_or_else(bad)?).map_err(|_| bad())?,
            days: u32::try_from(field(day, 1, 31, &[]).ok_or_else(bad)?).map_err(|_| bad())?,
            months: u16::try_from(field(month, 1, 12, &MONTHS).ok_or_else(bad)?)
                .map_err(|_| bad())?,
            weekdays: u8::try_from(weekdays).map_err(|_| bad())?,
            any_day: day.starts_with('*'),
            any_weekday: weekday.starts_with('*'),
        })
    }

    /// Whether some day of some year is due: a schedule naming only days its months never
    /// reach, such as the 31st of April, never is.
    fn fires(&self) -> bool {
        if !self.any_weekday && !self.any_day {
            return true; // Either matching is due, and some day of the week always comes.
        }
        (1..=12u32).filter(|m| self.months & 1 << m != 0).any(|m| {
            let longest = usize::try_from(m - 1).ok().and_then(|i| LONGEST.get(i)).copied();
            (1..=longest.unwrap_or(0)).any(|d| self.days & 1 << d != 0)
        })
    }
}

/// Checks a schedule's syntax, and that it is ever due.
pub(super) fn validate(schedule: &str) -> Result<(), SpecError> {
    if Schedule::parse(schedule)?.fires() {
        Ok(())
    } else {
        Err(SpecError::new("schedule", "names no day that ever comes"))
    }
}

#[cfg(test)]
mod tests;
