// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress budget (§5.1 of docs/05-scheduling.md, "The egress budget"): a bound on the bytes
//! the workloads an entry counts send beyond their links over a calendar month of UTC days,
//! enforced after use by shaping, since it is known only once it is metered. What a spent budget
//! does is the entry's own: a floor rate, or a cut-off.

use std::time::SystemTime;

/// What the workloads of an entry whose budget is spent are held to until the month ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhaustion {
    /// At most this many bits a second, beyond their links and on them.
    Floor(u64),
    /// Nothing leaves their links for beyond the node, replies included.
    CutOff,
}

impl Exhaustion {
    /// The stricter of the two: a cut-off before any floor, the lower floor before a higher, as
    /// a workload counted by several spent entries is held (§5.1).
    #[must_use]
    pub fn strictest(self, other: Self) -> Self {
        match (self, other) {
            (Self::CutOff, _) | (_, Self::CutOff) => Self::CutOff,
            (Self::Floor(a), Self::Floor(b)) => Self::Floor(a.min(b)),
        }
    }
}

/// An entry's egress budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressBudget {
    /// The bytes a month its workloads may send beyond their links.
    pub bytes_per_month: u64,
    /// What they are held to once those are spent.
    pub exhausted: Exhaustion,
    /// What they sent this month, as last reckoned; none before it is first reckoned.
    pub spent: Option<Spent>,
}

/// What the workloads an entry counts sent in a month, as the control plane last reckoned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spent {
    /// The month's first instant.
    pub month: SystemTime,
    /// The bytes.
    pub bytes: u64,
    /// Whether they reach the budget.
    pub exhausted: bool,
    /// When it was reckoned.
    pub reckoned_at: SystemTime,
}

/// Why a budget as asked cannot be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    /// A budget that changes nothing when it is spent is a number displayed and not enforced
    /// (Rule K-3).
    #[error("an egress budget names what is done when it is spent: a floor or a cut-off")]
    NothingWhenSpent,
    /// What is done when a budget is spent, with no budget to spend.
    #[error("an egress exhaustion names no budget to be spent")]
    NoBudget,
    /// A floor of nothing is a cut-off, and says so.
    #[error("a floor of zero is a cut-off: say cut_off")]
    ZeroFloor,
}

impl EgressBudget {
    /// The budget `bytes_per_month` and `exhausted` ask for, both or neither; none for neither.
    pub fn asked(
        bytes_per_month: Option<u64>,
        exhausted: Option<Exhaustion>,
    ) -> Result<Option<Self>, BudgetError> {
        match (bytes_per_month, exhausted) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(BudgetError::NothingWhenSpent),
            (None, Some(_)) => Err(BudgetError::NoBudget),
            (Some(_), Some(Exhaustion::Floor(0))) => Err(BudgetError::ZeroFloor),
            (Some(bytes_per_month), Some(exhausted)) => {
                Ok(Some(Self { bytes_per_month, exhausted, spent: None }))
            }
        }
    }
}

/// The calendar month of UTC days `t` falls in, its first instant and the next month's.
#[must_use]
pub fn month_of(t: SystemTime) -> (SystemTime, SystemTime) {
    let utc = jiff::tz::TimeZone::UTC;
    let Ok(at) = jiff::Timestamp::try_from(t) else { return (t, t) };
    let first = at.to_zoned(utc.clone()).date().first_of_month();
    let next = first.checked_add(jiff::Span::new().months(1)).unwrap_or(first);
    let instant = |d: jiff::civil::Date| {
        d.to_zoned(utc.clone()).map_or(t, |z| SystemTime::from(z.timestamp()))
    };
    (instant(first), instant(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_strictest_exhaustion_holds_and_a_budget_names_what_its_spending_does() {
        let (low, high) = (Exhaustion::Floor(1_000), Exhaustion::Floor(5_000));
        assert_eq!(low.strictest(high), low);
        assert_eq!(high.strictest(Exhaustion::CutOff), Exhaustion::CutOff);
        assert_eq!(EgressBudget::asked(None, None), Ok(None));
        assert_eq!(EgressBudget::asked(Some(9), None), Err(BudgetError::NothingWhenSpent));
        assert_eq!(EgressBudget::asked(None, Some(low)), Err(BudgetError::NoBudget));
        let zero = Some(Exhaustion::Floor(0));
        assert_eq!(EgressBudget::asked(Some(9), zero), Err(BudgetError::ZeroFloor));
        let held = EgressBudget::asked(Some(9), Some(Exhaustion::CutOff)).unwrap().unwrap();
        assert_eq!(held.exhausted, Exhaustion::CutOff);
    }

    #[test]
    fn a_month_is_a_calendar_month_of_utc_days() {
        // 2026-10-21T15:00:00Z: October, its 31 days.
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_594_800);
        let (start, next) = month_of(t);
        let secs = |s: SystemTime| s.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs(start), 1_790_812_800, "2026-10-01T00:00:00Z");
        assert_eq!(secs(next) - secs(start), 31 * 86_400);
        // 2028-02-15: a leap February.
        let feb = SystemTime::UNIX_EPOCH + Duration::from_secs(1_834_444_800);
        let (s, n) = month_of(feb);
        assert_eq!(secs(n) - secs(s), 29 * 86_400);
    }
}
