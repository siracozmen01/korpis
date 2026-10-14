// SPDX-License-Identifier: AGPL-3.0-or-later

//! What happens when a provider fails, and when it is merely slow (§5 of docs/16-extensions.md,
//! finding 23 of docs/23-walkthroughs.md). Each provider type declares how its dependents treat
//! its failure; each provider, by organization, type, and name, has one circuit breaker, which
//! trips on sustained failure and on sustained latency, judged from the calls it last finished.

use super::ProviderType;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How many finished calls, at most, a provider is judged by.
pub const WINDOW_CALLS: usize = 20;

/// How old a finished call may be and still be judged: an idle provider is judged afresh.
pub const WINDOW_AGE: Duration = Duration::from_secs(300);

/// The fewest calls a provider is judged by: two failures are not a pattern.
pub const MIN_CALLS: usize = 10;

/// The share of failed calls, in thousandths, that trips the breaker.
pub const FAILURE_TRIP: u32 = 500;

/// The median share of its deadline a call took, in thousandths, that trips the breaker: a
/// provider answering at nine tenths of every deadline never fails and still costs the fleet.
pub const LATENCY_TRIP: u32 = 800;

/// How long a breaker first stays open; each failed probe doubles it.
pub const FIRST_BACKOFF: Duration = Duration::from_secs(10);

/// The longest a breaker stays open before it probes again.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How a provider type's dependents treat its failure, declared and never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    /// What depends on it is observed `unsatisfiable`, the provider named in the reason, and
    /// reconciliation retries: a DNS record, a backup run, an edge route.
    Unsatisfiable,
    /// What it would have allowed is refused: failing open would bypass authentication.
    FailClosed,
    /// The install step that called it fails with its reason, and the plan stops there.
    StepFails,
    /// The condition it judges is reported unknown, never failed: a prober that is down must
    /// not get the workloads it probes restarted.
    Unknown,
    /// The request that needed it fails as unavailable and may be retried; nothing that does not
    /// use it is affected.
    Unavailable,
}

impl ProviderType {
    /// How its dependents treat its failure.
    #[must_use]
    pub const fn failure_policy(self) -> FailurePolicy {
        match self {
            Self::Dns | Self::BackupTarget | Self::EdgeRouter => FailurePolicy::Unsatisfiable,
            // A key it cannot open is a backup no one reads and one no one writes: there is no
            // other key to fall back to, and there must not be.
            Self::Identity | Self::KeyManagement => FailurePolicy::FailClosed,
            Self::Step => FailurePolicy::StepFails,
            Self::Health => FailurePolicy::Unknown,
            Self::Console | Self::ChatAdapter => FailurePolicy::Unavailable,
        }
    }
}

/// One finished call, as the breaker judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Whether it failed in a way that says the provider is unwell: a retryable failure,
    /// including its deadline passing and its stream ending, or an answer that answers nothing
    /// ([`CallFailure::unwell`](super::CallFailure::unwell)). A provider's answer that the
    /// request itself is wrong is a correct answer.
    pub failed: bool,
    /// From queueing to finishing, so the wait for a free slot counts.
    pub elapsed: Duration,
    /// From queueing to its deadline.
    pub allowed: Duration,
}

/// What the breaker measured of a provider's recent calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    /// Calls judged.
    pub calls: usize,
    /// Of which failed.
    pub failed: usize,
    /// The median time a call took.
    pub median_elapsed: Duration,
    /// The median share of its deadline a call took, in thousandths.
    pub median_share: u32,
}

/// Why a breaker tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trip {
    /// Too many calls failed.
    Failure,
    /// Calls succeed, too slowly.
    Latency,
}

impl Trip {
    /// Its name in events.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Failure => "failure",
            Self::Latency => "latency",
        }
    }
}

fn share(s: &Sample) -> u32 {
    if s.allowed.is_zero() {
        return 1000;
    }
    let thousandths = s.elapsed.as_micros().saturating_mul(1000) / s.allowed.as_micros().max(1);
    u32::try_from(thousandths.min(1000)).unwrap_or(1000)
}

/// Measures `samples`, the most recent first; at most [`WINDOW_CALLS`] are used.
#[must_use]
pub fn measure(samples: &[Sample]) -> Measured {
    let window: Vec<&Sample> = samples.iter().take(WINDOW_CALLS).collect();
    let mut elapsed: Vec<Duration> = window.iter().map(|s| s.elapsed).collect();
    let mut shares: Vec<u32> = window.iter().map(|s| share(s)).collect();
    elapsed.sort_unstable();
    shares.sort_unstable();
    let middle = window.len() / 2;
    Measured {
        calls: window.len(),
        failed: window.iter().filter(|s| s.failed).count(),
        median_elapsed: elapsed.get(middle).copied().unwrap_or_default(),
        median_share: shares.get(middle).copied().unwrap_or(0),
    }
}

/// Whether a closed breaker trips on what was measured.
#[must_use]
pub fn judge(m: &Measured) -> Option<Trip> {
    if m.calls < MIN_CALLS {
        return None;
    }
    let failed = u32::try_from(m.failed * 1000 / m.calls).unwrap_or(1000);
    if failed >= FAILURE_TRIP {
        Some(Trip::Failure)
    } else if m.median_share >= LATENCY_TRIP {
        Some(Trip::Latency)
    } else {
        None
    }
}

/// Whether one probe's call says the provider recovered: it neither failed nor took the share
/// of its deadline that trips the breaker.
#[must_use]
pub fn recovered(probe: &Sample) -> bool {
    !probe.failed && share(probe) < LATENCY_TRIP
}

/// A provider's breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breaker {
    /// Calls go through.
    Closed,
    /// Calls fail at once until `until`; then one probe goes through.
    Open {
        /// When it probes.
        until: SystemTime,
        /// How long it stayed open this time, which a failed probe doubles.
        backoff: Duration,
    },
    /// One call, the probe, is in flight; the rest fail at once until it finishes.
    HalfOpen {
        /// The probe.
        probe: Uuid,
        /// When the probe's own deadline passes; a probe not judged by then was lost (its
        /// caller died), and the next call probes instead.
        until: SystemTime,
        /// The backoff it was opened with.
        backoff: Duration,
    },
}

/// What a breaker says to one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// It goes through.
    Admit,
    /// It goes through as the probe; the breaker is now half open.
    Probe(Breaker),
    /// It fails at once; retrying after `until` may succeed.
    Refuse {
        /// When the breaker next lets a call through.
        until: SystemTime,
    },
}

impl Breaker {
    /// Whether `call`, with `deadline`, may go through at `now`.
    #[must_use]
    pub fn admit(self, call: Uuid, deadline: SystemTime, now: SystemTime) -> Admission {
        match self {
            Self::Closed => Admission::Admit,
            Self::Open { until, .. } if now < until => Admission::Refuse { until },
            Self::HalfOpen { until, .. } if now <= until => Admission::Refuse { until },
            Self::Open { backoff, .. } | Self::HalfOpen { backoff, .. } => {
                Admission::Probe(Self::HalfOpen { probe: call, until: deadline, backoff })
            }
        }
    }

    /// Opened at `now` after a trip.
    #[must_use]
    pub fn tripped(now: SystemTime) -> Self {
        Self::Open { until: now + FIRST_BACKOFF, backoff: FIRST_BACKOFF }
    }

    /// After the probe's call finished at `now`: closed if it says the provider recovered,
    /// otherwise open again for twice as long, up to [`MAX_BACKOFF`].
    #[must_use]
    pub fn probed(self, sample: &Sample, now: SystemTime) -> Self {
        let backoff = match self {
            Self::HalfOpen { backoff, .. } | Self::Open { backoff, .. } => backoff,
            Self::Closed => FIRST_BACKOFF,
        };
        if recovered(sample) {
            return Self::Closed;
        }
        let backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        Self::Open { until: now + backoff, backoff }
    }
}

#[cfg(test)]
#[path = "breaker_tests.rs"]
mod tests;
