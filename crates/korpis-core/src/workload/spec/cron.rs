// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cron schedules: five fields (minute, hour, day of month, month, day of week), each a list of
//! `*`, `n`, or `n-m`, optionally stepped with `/k`. Months and days of the week may be named.

use super::SpecError;

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

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

fn item(s: &str, lo: u32, hi: u32, names: &[&str]) -> bool {
    let (range, step) = s.split_once('/').map_or((s, None), |(r, k)| (r, Some(k)));
    if step.is_some_and(|k| k.parse::<u32>().map_or(true, |k| k == 0 || k > hi)) {
        return false;
    }
    if range == "*" {
        return true;
    }
    match range.split_once('-') {
        Some((a, b)) => {
            matches!((value(a, lo, hi, names), value(b, lo, hi, names)), (Some(a), Some(b)) if a <= b)
        }
        None => value(range, lo, hi, names).is_some(),
    }
}

/// Checks a schedule's syntax.
pub(super) fn validate(schedule: &str) -> Result<(), SpecError> {
    let ranges: [(u32, u32, &[&str]); 5] =
        [(0, 59, &[]), (0, 23, &[]), (1, 31, &[]), (1, 12, &MONTHS), (0, 7, &DAYS)];
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    let ok = fields.len() == 5
        && fields
            .iter()
            .zip(ranges)
            .all(|(f, (lo, hi, names))| f.split(',').all(|i| item(i, lo, hi, names)));
    if ok { Ok(()) } else { Err(SpecError::new("schedule", "five cron fields")) }
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn five_fields_in_range() {
        for ok in ["0 4 * * *", "*/15 * * * mon-fri", "0 0 1 jan,jul *", "5,35 8-18/2 * * 0"] {
            assert!(validate(ok).is_ok(), "{ok}");
        }
        for bad in
            ["", "* * * *", "60 * * * *", "* 24 * * *", "*/0 * * * *", "5-1 * * * *", "@daily"]
        {
            assert!(validate(bad).is_err(), "{bad}");
        }
    }
}
