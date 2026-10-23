// SPDX-License-Identifier: AGPL-3.0-or-later

//! Judging a workload's health from its probes (§5 of docs/04-runtimes.md): when each probe is
//! tried, and what its attempts add up to. A probe passes at its first success and fails only
//! after `failures` attempts in a row have failed, so one slow answer is not a verdict; before
//! either it is unjudged. The node that runs the probes judges them, with these rules, and the
//! workload's health is its liveness verdict, or its readiness verdict where it declares no
//! liveness probe.

use super::observation::Health;
use super::spec::health::{Probe, ProbeKind};
use std::time::{Duration, Instant};

/// How often a probe is tried when it does not say.
pub const PERIOD: Duration = Duration::from_secs(10);
/// How long an attempt may take when the probe does not say; never longer than its period.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How many attempts in a row must fail before the probe fails, when it does not say.
pub const FAILURES: u32 = 3;
/// The shortest period: a probe that asks for less is tried every second, so that no recipe
/// turns the node's prober into a load on the workload it judges.
pub const SHORTEST_PERIOD: Duration = Duration::from_secs(1);
/// The shortest period of a `query` probe: each of its rounds is a call to its health provider
/// through the control plane's queue (§5 of docs/16-extensions.md), which a period of a second
/// across a fleet would turn into the queue's main load.
pub const SHORTEST_QUERY_PERIOD: Duration = Duration::from_secs(5);

/// When a probe is tried, with its defaults filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How often.
    pub period: Duration,
    /// How long one attempt may take.
    pub timeout: Duration,
    /// Failures in a row before it fails.
    pub failures: u32,
    /// How long after the workload starts before the first attempt.
    pub initial_delay: Duration,
}

/// `d`, unless it is absent or zero.
fn given(d: Option<Duration>) -> Option<Duration> {
    d.filter(|d| !d.is_zero())
}

impl Timing {
    /// The timing of `p`: what it sets, and the defaults for what it does not.
    #[must_use]
    pub fn of(p: &Probe) -> Self {
        let timeout = given(p.timeout);
        let period = given(p.period).unwrap_or_else(|| PERIOD.max(timeout.unwrap_or_default()));
        let shortest = match p.kind {
            ProbeKind::Query { .. } => SHORTEST_QUERY_PERIOD,
            _ => SHORTEST_PERIOD,
        };
        let period = period.max(shortest);
        Self {
            period,
            timeout: timeout.unwrap_or(TIMEOUT).min(period),
            failures: if p.failures == 0 { FAILURES } else { p.failures },
            initial_delay: p.initial_delay.unwrap_or_default(),
        }
    }
}

/// What a probe's attempts add up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verdict {
    /// Neither passed nor failed yet.
    #[default]
    Unjudged,
    /// Its last attempt passed.
    Passing,
    /// Its last `failures` attempts failed.
    Failing,
}

/// One probe's verdict, kept across its attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judge {
    needed: u32,
    failed: u32,
    verdict: Verdict,
}

impl Judge {
    /// A judge that fails a probe after `failures` attempts in a row fail; at least one.
    #[must_use]
    pub const fn new(failures: u32) -> Self {
        let needed = if failures == 0 { 1 } else { failures };
        Self { needed, failed: 0, verdict: Verdict::Unjudged }
    }

    /// The verdict so far.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// Records an attempt that could not be judged, its health provider having failed (§5 of
    /// docs/16-extensions.md): the verdict is unknown, never failing, and the failures counted so
    /// far are forgotten; whether the verdict changed.
    pub fn unknown(&mut self) -> bool {
        let before = self.verdict;
        self.failed = 0;
        self.verdict = Verdict::Unjudged;
        before != self.verdict
    }

    /// Records one attempt; whether the verdict changed.
    pub fn record(&mut self, passed: bool) -> bool {
        let before = self.verdict;
        if passed {
            self.failed = 0;
            self.verdict = Verdict::Passing;
        } else {
            self.failed = self.failed.saturating_add(1);
            if self.failed >= self.needed {
                self.verdict = Verdict::Failing;
            }
        }
        before != self.verdict
    }
}

/// What a `heartbeat` probe finds when it is checked at `now` (§5 of docs/04-runtimes.md): passed
/// when the workload's last beat in this life of its object, `last`, came within `max_interval`;
/// failed when it is older, or when none came within `max_interval` of `from`, the life's start
/// and its initial delay; and nothing yet while the workload has not been silent that long, since
/// a workload that is still starting has not missed a beat.
#[must_use]
pub fn heard(
    last: Option<Instant>,
    from: Instant,
    max_interval: Duration,
    now: Instant,
) -> Option<bool> {
    match last {
        Some(beat) => Some(now.saturating_duration_since(beat) <= max_interval),
        None if now.saturating_duration_since(from) <= max_interval => None,
        None => Some(false),
    }
}

/// The health a workload's verdicts add up to: its liveness probe's where it has one, and
/// otherwise its readiness probe's; unknown where neither is judged, or there is neither.
#[must_use]
pub fn health(liveness: Option<Verdict>, readiness: Option<Verdict>) -> Health {
    match liveness.or(readiness) {
        Some(Verdict::Passing) => Health::Healthy,
        Some(Verdict::Failing) => Health::Unhealthy,
        Some(Verdict::Unjudged) | None => Health::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(period: u64, timeout: u64, failures: u32) -> Probe {
        let secs = |s| (s > 0).then(|| Duration::from_secs(s));
        Probe {
            kind: ProbeKind::Tcp { endpoint: "e".into() },
            period: secs(period),
            timeout: secs(timeout),
            failures,
            initial_delay: None,
        }
    }

    #[test]
    fn what_a_probe_does_not_set_is_the_stated_default() {
        let t = Timing::of(&probe(0, 0, 0));
        assert_eq!((t.period, t.timeout, t.failures), (PERIOD, TIMEOUT, FAILURES));
        assert_eq!(t.initial_delay, Duration::ZERO);
        let t = Timing::of(&probe(2, 0, 1));
        assert_eq!((t.period, t.timeout), (Duration::from_secs(2), Duration::from_secs(2)));
        let t = Timing::of(&probe(0, 30, 0));
        assert_eq!(t.period, Duration::from_secs(30), "a period never shorter than its timeout");
        let mut fast = probe(0, 0, 0);
        fast.period = Some(Duration::from_millis(10));
        assert_eq!(Timing::of(&fast).period, SHORTEST_PERIOD);
        fast.kind = ProbeKind::Query { protocol: "korpis/minecraft".into(), endpoint: "e".into() };
        assert_eq!(Timing::of(&fast).period, SHORTEST_QUERY_PERIOD, "a query asks a provider");
    }

    #[test]
    fn a_probe_passes_at_once_and_fails_only_after_its_failures_in_a_row() {
        let mut j = Judge::new(3);
        assert!(!j.record(false), "one failure judges nothing");
        assert_eq!(j.verdict(), Verdict::Unjudged);
        assert!(j.record(true));
        assert!(!j.record(false) && !j.record(false));
        assert_eq!(j.verdict(), Verdict::Passing, "two failures do not undo a pass");
        assert!(j.record(false));
        assert_eq!(j.verdict(), Verdict::Failing);
        assert!(j.record(true), "and one success restores it");
        assert!(j.unknown() && j.verdict() == Verdict::Unjudged, "an unjudged attempt");
        assert!(!j.record(false), "and the failures before it are forgotten");
    }

    #[test]
    fn a_heartbeat_fails_on_silence_longer_than_its_interval_and_not_before() {
        let from = Instant::now();
        let at = |s: u64| from + Duration::from_secs(s);
        let max = Duration::from_secs(30);
        assert_eq!(heard(None, from, max, at(20)), None, "still starting");
        assert_eq!(heard(None, from, max, at(31)), Some(false), "never heard from");
        assert_eq!(heard(Some(at(10)), from, max, at(35)), Some(true));
        assert_eq!(heard(Some(at(10)), from, max, at(41)), Some(false), "silent since");
    }

    #[test]
    fn health_is_liveness_then_readiness() {
        use Verdict::{Failing, Passing, Unjudged};
        assert_eq!(health(Some(Failing), Some(Passing)), Health::Unhealthy);
        assert_eq!(health(None, Some(Passing)), Health::Healthy);
        assert_eq!(health(Some(Unjudged), Some(Passing)), Health::Unknown);
        assert_eq!(health(None, None), Health::Unknown);
    }
}
