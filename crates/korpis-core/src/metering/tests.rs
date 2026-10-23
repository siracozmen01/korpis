// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

const HOUR: Duration = Duration::from_secs(3600);

fn t(hours: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        + Duration::from_secs(1_760_000_000 / 3600 * 3600)
        + HOUR * u32::try_from(hours).unwrap()
}

fn sample(hour: u64, cpu: u64) -> Sample {
    Sample {
        workload: Uuid::nil(),
        interval_start: t(hour),
        interval: HOUR,
        lease_epoch: 1,
        counters: Counters { cpu_usec: Some(cpu), ..Counters::default() },
        memory_peak_bytes: None,
        disk_bytes: None,
        disk_allocated: false,
        losses: Vec::new(),
    }
}

fn reading(hour: u64, cpu: u64) -> Reading {
    Reading { end: t(hour), counters: Counters { cpu_usec: Some(cpu), ..Counters::default() } }
}

#[test]
fn an_interval_is_the_difference_of_two_samples() {
    let m = measure(&sample(1, 900), Some(&reading(1, 600))).unwrap();
    assert_eq!((m.from, m.to, m.cause), (t(1), t(2), None));
    assert_eq!(m.used.cpu_usec, Some(300));
    assert_eq!(m.used.io_read_bytes, None, "what nothing measured stays unmeasured");
}

#[test]
fn the_first_sample_on_a_node_counts_from_zero() {
    let m = measure(&sample(4, 250), None).unwrap();
    assert_eq!((m.from, m.used.cpu_usec), (t(4), Some(250)));
}

#[test]
fn lost_samples_leave_one_record_spanning_them() {
    let m = measure(&sample(5, 1_000), Some(&reading(2, 400))).unwrap();
    assert_eq!((m.from, m.to), (t(2), t(6)), "from the last received to this one's end");
    assert_eq!(
        (m.used.cpu_usec, m.cause),
        (Some(600), Some(SAMPLES_LOST)),
        "measured, not divided"
    );
}

#[test]
fn a_count_that_began_again_is_counted_from_its_own_start() {
    let m = measure(&sample(3, 50), Some(&reading(3, 9_000))).unwrap();
    assert_eq!((m.from, m.used.cpu_usec, m.cause), (t(3), Some(50), Some(COUNTER_RESET)));
}

#[test]
fn a_sample_no_later_than_the_last_measures_nothing() {
    assert_eq!(measure(&sample(1, 900), Some(&reading(2, 600))), Err(Unmeasured::Stale));
}

#[test]
fn a_counter_first_measured_now_is_counted_from_zero() {
    let before = Counters { cpu_usec: Some(10), ..Counters::default() };
    let now = Counters { cpu_usec: Some(15), net_rx_bytes: Some(70), ..Counters::default() };
    let used = now.since(&before);
    assert_eq!((used.cpu_usec, used.net_rx_bytes), (Some(5), Some(70)));
    assert!(!now.below(&before));
}

#[test]
fn a_silent_span_is_one_gap_an_interval_and_a_day_is_utc() {
    let spans = gaps(t(2), t(5), HOUR);
    assert_eq!(spans, [(t(2), t(3)), (t(3), t(4)), (t(4), t(5))]);
    assert!(gaps(t(2), t(2) + HOUR / 2, HOUR).is_empty(), "an interval not ended is no gap");
    let midday = SystemTime::UNIX_EPOCH + DAY * 20_000 + HOUR * 13;
    assert_eq!(day_of(midday), SystemTime::UNIX_EPOCH + DAY * 20_000);
}

#[test]
fn a_sample_no_store_could_hold_or_far_from_the_clock_is_implausible() {
    let now = t(10);
    assert_eq!(implausible(&sample(5, 100), now), None);
    let mut long = sample(5, 100);
    long.interval = Duration::MAX;
    assert!(implausible(&long, now).is_some(), "an interval that does not add");
    assert_eq!(long.end(), long.interval_start, "and its end does not panic");
    assert!(implausible(&sample(20, 100), now).is_some(), "ten hours ahead of the clock");
    let huge = sample(5, u64::MAX);
    assert!(implausible(&huge, now).is_some(), "a counter beyond what is stored");
    let mut lost = sample(5, 100);
    lost.losses.push(Loss { from: SystemTime::UNIX_EPOCH, to: t(5), cause: "x".into() });
    assert!(implausible(&lost, now).is_some(), "a loss far from the clock");
}
