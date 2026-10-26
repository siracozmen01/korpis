// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn sample(failed: bool, elapsed_ms: u64) -> Sample {
    Sample {
        failed,
        elapsed: Duration::from_millis(elapsed_ms),
        allowed: Duration::from_millis(5000),
    }
}

#[test]
fn a_provider_that_answers_at_nine_tenths_of_every_deadline_trips_on_latency() {
    let slow = vec![sample(false, 4500); 12];
    let m = measure(&slow);
    assert_eq!((m.calls, m.failed, m.median_share), (12, 0, 900));
    assert_eq!(judge(&m), Some(Trip::Latency), "finding 23: success can be the failure");
    let brisk = vec![sample(false, 500); 12];
    assert_eq!(judge(&measure(&brisk)), None);
}

#[test]
fn half_failing_trips_and_a_few_calls_are_not_a_pattern() {
    let mut calls = vec![sample(true, 5000); 5];
    calls.extend(vec![sample(false, 100); 5]);
    assert_eq!(judge(&measure(&calls)), Some(Trip::Failure));
    assert_eq!(judge(&measure(&calls[..9])), None, "fewer than the minimum");
    let mostly_fine = [vec![sample(true, 100); 4], vec![sample(false, 100); 16]].concat();
    assert_eq!(judge(&measure(&mostly_fine)), None);
}

#[test]
fn only_the_most_recent_window_is_judged() {
    let calls = [vec![sample(false, 100); WINDOW_CALLS], vec![sample(true, 5000); 30]].concat();
    let m = measure(&calls);
    assert_eq!((m.calls, m.failed), (WINDOW_CALLS, 0), "older calls fall out of the window");
}

#[test]
fn an_open_breaker_refuses_then_probes_once_then_closes_or_backs_off() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let open = Breaker::tripped(t0);
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let deadline = t0 + Duration::from_secs(60);
    assert_eq!(open.admit(a, deadline, t0), Admission::Refuse { until: t0 + FIRST_BACKOFF });
    let later = t0 + FIRST_BACKOFF;
    let Admission::Probe(half) = open.admit(a, deadline, later) else { panic!("a probe") };
    assert!(matches!(half.admit(b, deadline, later), Admission::Refuse { .. }), "one probe");
    assert_eq!(half.probed(&sample(false, 100), later), Breaker::Closed);
    let again = half.probed(&sample(true, 5000), later);
    assert_eq!(
        again,
        Breaker::Open { until: later + FIRST_BACKOFF * 2, backoff: FIRST_BACKOFF * 2 }
    );
    let slow = half.probed(&sample(false, 4900), later);
    assert!(matches!(slow, Breaker::Open { .. }), "a slow success is not a recovery");
}

#[test]
fn a_lost_probe_is_replaced_and_the_backoff_is_bounded() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let half = Breaker::HalfOpen { probe: a, until: t0, backoff: MAX_BACKOFF };
    let after = t0 + Duration::from_secs(1);
    let Admission::Probe(Breaker::HalfOpen { probe, .. }) = half.admit(b, after, after) else {
        panic!("the next call probes instead");
    };
    assert_eq!(probe, b);
    let Breaker::Open { backoff, .. } = half.probed(&sample(true, 1), after) else {
        panic!("open again");
    };
    assert_eq!(backoff, MAX_BACKOFF);
}

#[test]
fn every_type_declares_how_its_failure_is_treated() {
    use FailurePolicy as P;
    let declared: Vec<_> = ProviderType::ALL.iter().map(|t| t.failure_policy()).collect();
    assert_eq!(
        declared,
        [
            P::Unsatisfiable,
            P::Unsatisfiable,
            P::StepFails,
            P::Unknown,
            P::Unavailable,
            P::FailClosed,
            P::Unsatisfiable,
            P::Unavailable,
            P::FailClosed,
            P::FailClosed,
        ]
    );
}
