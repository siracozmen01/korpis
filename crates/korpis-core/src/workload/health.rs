// SPDX-License-Identifier: AGPL-3.0-or-later

//! Judging a workload's health from its probes (§5 of docs/04-runtimes.md): when each probe is
//! tried, and what its attempts add up to. A probe passes at its first success and fails only
//! after `failures` attempts in a row have failed, so one slow answer is not a verdict; before
//! either it is unjudged. The node that runs the probes judges them, with these rules, and the
//! workload's health is its liveness verdict, or its readiness verdict where it declares no
//! liveness probe.

use super::observation::Health;
use super::spec::health::Probe;
use std::time::Duration;

/// How often a probe is tried when it does not say.
pub const PERIOD: Duration = Duration::from_secs(10);
/// How long an attempt may take when the probe does not say; never longer than its period.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How many attempts in a row must fail before the probe fails, when it does not say.
pub const FAILURES: u32 = 3;
/// The shortest period: a probe that asks for less is tried every second, so that no recipe
/// turns the node's prober into a load on the workload it judges.
pub const SHORTEST_PERIOD: Duration = Duration::from_secs(1);

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
        let period = period.max(SHORTEST_PERIOD);
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
    use crate::workload::spec::health::ProbeKind;

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
