// SPDX-License-Identifier: AGPL-3.0-or-later

//! Coalescing repeated identical denials (§10 of docs/10-api.md, §4 of
//! docs/15-observability.md). Every denial is recorded, but a client refused the same action on
//! the same object over and over must not turn the effect log into an unbounded write path. The
//! first denial of a run is written at once, so an auditor sees it as it happens; the repeats
//! within the window that follows are counted, and written as one effect with the count and the
//! window's bounds when the window closes. The run is keyed by subject, action, and object, so a
//! different refusal is never folded into another.

use std::time::{Duration, SystemTime};

/// The window a run of identical denials is counted over, from the denial written at once.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// A run of identical denials: the one written when it opened, and the repeats counted since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenialRun {
    /// When the denial written at once happened; the window runs from here.
    pub opened: SystemTime,
    /// Repeats counted and not yet written.
    pub repeats: u64,
    /// The first repeat, if any.
    pub first: Option<SystemTime>,
    /// The last repeat, if any.
    pub last: Option<SystemTime>,
}

/// What to do with a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Count it in the open run, which becomes `run`; write nothing now.
    Count {
        /// The run with this denial counted.
        run: DenialRun,
    },
    /// Write it, after writing the closed run's repeats if it had any, and open a new run.
    Write {
        /// The closed run, to be written as one effect if it counted any repeats.
        closed: Option<DenialRun>,
        /// The run this denial opens.
        run: DenialRun,
    },
}

impl DenialRun {
    /// The run a denial written at `now` opens.
    #[must_use]
    pub const fn open(now: SystemTime) -> Self {
        Self { opened: now, repeats: 0, first: None, last: None }
    }

    /// Whether its window has closed by `now`.
    #[must_use]
    pub fn closed(&self, now: SystemTime, window: Duration) -> bool {
        self.opened.checked_add(window).is_none_or(|end| now >= end)
    }

    /// The repeats to write when it closes: their count and the bounds of the window they
    /// span, or `None` if nothing repeated.
    #[must_use]
    pub fn repeats(&self) -> Option<(u64, SystemTime, SystemTime)> {
        match (self.repeats, self.first, self.last) {
            (0, _, _) | (_, None, _) | (_, _, None) => None,
            (n, Some(first), Some(last)) => Some((n, first, last)),
        }
    }

    fn counted(self, now: SystemTime) -> Self {
        Self {
            repeats: self.repeats.saturating_add(1),
            first: self.first.or(Some(now)),
            last: Some(now),
            ..self
        }
    }
}

/// Decides what a denial at `now` does, given the open run for its subject, action, and object.
#[must_use]
pub fn offer(open: Option<DenialRun>, now: SystemTime, window: Duration) -> Offer {
    match open {
        Some(run) if !run.closed(now, window) => Offer::Count { run: run.counted(now) },
        closed => Offer::Write { closed, run: DenialRun::open(now) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + s)
    }

    #[test]
    fn the_first_denial_is_written_and_its_repeats_are_counted_until_the_window_closes() {
        let w = Duration::from_secs(60);
        let Offer::Write { closed: None, run } = offer(None, at(0), w) else { panic!() };
        let Offer::Count { run } = offer(Some(run), at(5), w) else { panic!() };
        let Offer::Count { run } = offer(Some(run), at(59), w) else { panic!() };
        assert_eq!(run.repeats(), Some((2, at(5), at(59))));
        let Offer::Write { closed: Some(closed), run } = offer(Some(run), at(60), w) else {
            panic!()
        };
        assert_eq!(closed.repeats(), Some((2, at(5), at(59))), "written once, with its bounds");
        assert_eq!(run, DenialRun::open(at(60)), "and this denial opens the next run");
    }

    #[test]
    fn a_run_nothing_repeated_in_writes_nothing_when_it_closes() {
        let run = DenialRun::open(at(0));
        assert!(run.closed(at(60), Duration::from_secs(60)));
        assert!(!run.closed(at(59), Duration::from_secs(60)));
        assert_eq!(run.repeats(), None);
    }
}
