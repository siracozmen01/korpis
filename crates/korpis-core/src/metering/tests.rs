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
