// SPDX-License-Identifier: AGPL-3.0-or-later

//! Coalescing repeated identical effects into one record with a count (§4.10 of
//! docs/02-architecture.md), so a crash loop's restarts do not fill the journal one record at a
//! time. The first of a run is written at once; repeats within the window are counted in memory
//! and written as one record when the window passes or something else is written. Only effects
//! are coalesced: a record whose order matters, such as a break-glass session or a lease
//! release, is always written before the append returns.

use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use std::time::{Duration, SystemTime};

/// Repeats counted and not yet written.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pending {
    /// What repeated.
    pub(super) body: Body,
    /// When the first repeat happened.
    pub(super) first_at: SystemTime,
    /// How many repeats.
    pub(super) count: u64,
}

/// What to do with a record offered.
#[derive(Debug, PartialEq)]
pub(super) enum Offer {
    /// Write it, after writing `flush` if there is one.
    Write {
        /// Repeats to write first, so the journal keeps the order things happened in.
        flush: Option<Pending>,
    },
    /// It was counted; nothing is written now, except `flush` if there is one.
    Held {
        /// Repeats of something else, now ended, to write.
        flush: Option<Pending>,
    },
}

/// The coalescer.
#[derive(Debug)]
pub(super) struct Coalescer {
    window: Duration,
    last: Option<(Body, SystemTime)>,
    pending: Option<Pending>,
}

fn within(since: SystemTime, now: SystemTime, window: Duration) -> bool {
    now.duration_since(since).unwrap_or_default() < window
}

impl Coalescer {
    /// Coalescing repeats within `window`; a zero window coalesces nothing.
    pub(super) fn new(window: Duration) -> Self {
        Self { window, last: None, pending: None }
    }

    /// Offers `body`, happening at `now`.
    pub(super) fn offer(&mut self, body: &Body, now: SystemTime) -> Offer {
        if !matches!(body, Body::Effect(_)) || self.window.is_zero() {
            return Offer::Write { flush: self.pending.take() };
        }
        if let Some(p) = self.pending.as_mut().filter(|p| &p.body == body) {
            p.count = p.count.saturating_add(1);
            return Offer::Held { flush: None };
        }
        let flush = self.pending.take();
        let repeats =
            self.last.as_ref().is_some_and(|(b, at)| b == body && within(*at, now, self.window));
        if repeats && flush.is_none() {
            self.pending = Some(Pending { body: body.clone(), first_at: now, count: 1 });
            return Offer::Held { flush: None };
        }
        Offer::Write { flush }
    }

    /// Notes that `body` was written at `now`: repeats of it within the window are counted.
    pub(super) fn written(&mut self, body: &Body, now: SystemTime) {
        self.last = matches!(body, Body::Effect(_)).then(|| (body.clone(), now));
    }

    /// The repeats whose window has passed, to write now.
    pub(super) fn due(&mut self, now: SystemTime) -> Option<Pending> {
        let last = self.last.as_ref().map(|(_, at)| *at)?;
        if self.pending.is_some() && !within(last, now, self.window) {
            return self.pending.take();
        }
        None
    }

    /// The repeats not yet written, whatever their window: for a clean shutdown.
    pub(super) fn drain(&mut self) -> Option<Pending> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    fn effect(action: &str) -> Body {
        Body::Effect(Box::new(pb::Effect { action: action.into(), ..Default::default() }))
    }

    #[test]
    fn a_run_of_repeats_is_one_record_with_a_count_written_when_the_window_passes() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let later = |s| t + Duration::from_secs(s);
        let mut c = Coalescer::new(Duration::from_secs(10));
        let restart = effect("workload.restart");
        assert_eq!(c.offer(&restart, t), Offer::Write { flush: None });
        c.written(&restart, t);
        assert_eq!(c.offer(&restart, later(1)), Offer::Held { flush: None });
        assert_eq!(c.offer(&restart, later(2)), Offer::Held { flush: None });
        assert_eq!(c.due(later(5)), None, "the window is still open");
        let due = c.due(later(11)).unwrap();
        assert_eq!((due.count, due.first_at), (2, later(1)));
        c.written(&due.body, later(11));
        assert_eq!(c.offer(&restart, later(12)), Offer::Held { flush: None });
        let other = effect("workload.start");
        let Offer::Write { flush: Some(p) } = c.offer(&other, later(13)) else {
            panic!("something else ends the run and is written after it");
        };
        assert_eq!(p.count, 1);
    }

    #[test]
    fn records_whose_order_matters_are_never_held() {
        let t = SystemTime::UNIX_EPOCH;
        let mut c = Coalescer::new(Duration::from_secs(10));
        let released = Body::LeaseReleased(Box::default());
        c.written(&released, t);
        assert_eq!(c.offer(&released, t), Offer::Write { flush: None });
    }
}
