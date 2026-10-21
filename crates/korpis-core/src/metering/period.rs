// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering's periods (§2 of docs/15-observability.md): one UTC day each, open while it runs,
//! closing until its lateness window passes, and closed when the control plane closed it.

use super::DAY;
use std::time::{Duration, SystemTime};

/// Where a day of metering stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodState {
    /// Its day is running.
    Open,
    /// Its day ended, and a late sample may still revise it.
    Closing,
    /// Closed: a later record of it is an adjustment.
    Closed,
}

/// One day of metering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    /// Its start.
    pub start: SystemTime,
    /// Its state.
    pub state: PeriodState,
    /// When it closed, or when it will at the lateness window in force.
    pub closes_at: SystemTime,
}

impl Period {
    /// The day beginning `start`, closed at `closed` if it was, at `now` under `lateness`.
    #[must_use]
    pub fn of(
        start: SystemTime,
        closed: Option<SystemTime>,
        now: SystemTime,
        lateness: Duration,
    ) -> Self {
        let end = start + DAY;
        let (state, closes_at) = match closed {
            Some(at) => (PeriodState::Closed, at),
            None if now < end => (PeriodState::Open, end + lateness),
            None => (PeriodState::Closing, end + lateness),
        };
        Self { start, state, closes_at }
    }

    /// Its end.
    #[must_use]
    pub fn end(&self) -> SystemTime {
        self.start + DAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_open_while_it_runs_closing_after_and_closed_once_closed() {
        let day = SystemTime::UNIX_EPOCH + DAY * 20_000;
        let lateness = Duration::from_secs(3600);
        let open = Period::of(day, None, day + DAY / 2, lateness);
        assert_eq!((open.state, open.closes_at), (PeriodState::Open, day + DAY + lateness));
        assert_eq!(Period::of(day, None, day + DAY, lateness).state, PeriodState::Closing);
        let at = day + DAY * 2;
        let closed = Period::of(day, Some(at), day + DAY * 3, lateness);
        assert_eq!((closed.state, closed.closes_at), (PeriodState::Closed, at));
    }
}
