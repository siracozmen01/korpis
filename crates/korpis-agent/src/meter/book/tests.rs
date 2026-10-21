// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn cpu(usec: u64) -> agent::MeterCounters {
    agent::MeterCounters { cpu_usec: Some(usec), ..Default::default() }
}

fn t(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000 + seconds)
}

fn read(book: &mut Book, source: &str, usec: u64, at: u64) {
    book.read("wl_1", source, cpu(usec), t(at));
    book.read_all(t(at));
}

#[test]
fn a_workload_s_counter_runs_on_across_the_objects_it_runs_as() {
    let mut book = Book::default();
    read(&mut book, "mem-1", 100, 0);
    read(&mut book, "mem-1", 400, 10);
    book.ended("wl_1", "mem-1", Some(cpu(450)), t(15));
    read(&mut book, "mem-2", 30, 20);
    let closed = book.close("wl_1");
    assert_eq!(
        closed.counters.cpu_usec,
        Some(480),
        "the first object's whole life and the second's"
    );
    assert!(closed.losses.is_empty());
    assert_eq!(closed.counters.io_read_bytes, None, "what nothing measured stays unmeasured");
}

#[test]
fn a_reading_that_went_back_is_a_source_made_again_unseen() {
    let mut book = Book::default();
    read(&mut book, "mem-1", 500, 0);
    read(&mut book, "mem-1", 20, 10);
    let closed = book.close("wl_1");
    assert_eq!(closed.counters.cpu_usec, Some(520), "nothing read is lost, nothing counted twice");
    assert_eq!(closed.losses.len(), 1);
    assert_eq!(closed.losses[0].cause, COUNTER_RESET);
    assert_eq!(when(closed.losses[0].from.as_option().unwrap()), t(0));
    assert!(book.close("wl_1").losses.is_empty(), "a loss is said once");
}

#[test]
fn an_object_that_ended_unread_keeps_its_last_reading_and_says_what_is_lost() {
    let mut book = Book::default();
    read(&mut book, "mem-1", 300, 0);
    book.ended("wl_1", "mem-1", None, t(7));
    let closed = book.close("wl_1");
    assert_eq!(closed.counters.cpu_usec, Some(300));
    assert_eq!(closed.losses[0].cause, ENDED_UNREAD);
    assert!(!book.running("wl_1"));
}

#[test]
fn a_reboot_ends_every_source_at_its_last_reading() {
    let mut book = Book::default();
    read(&mut book, "mem-1", 300, 0);
    book.read(
        "wl_1",
        "net",
        agent::MeterCounters { net_rx_bytes: Some(9), ..Default::default() },
        t(0),
    );
    book.rebooted(t(60));
    read(&mut book, "mem-1", 5, 70);
    let closed = book.close("wl_1");
    assert_eq!(closed.counters.cpu_usec, Some(305), "the new cgroup's count is added, not a reset");
    assert_eq!(closed.counters.net_rx_bytes, Some(9));
    let causes: Vec<_> = closed.losses.iter().map(|l| l.cause.as_str()).collect();
    assert_eq!(causes, [NODE_REBOOTED], "the restart is the loss, not a counter reset after it");
}

#[test]
fn memory_is_integrated_and_an_interval_s_peak_falls_with_the_next() {
    let mut book = Book::default();
    let gib = 1 << 30;
    book.memory(("wl_1", "mem-1"), (Some(gib), Some(2 * gib)), Duration::from_secs(10));
    book.memory(("wl_1", "mem-1"), (Some(gib), Some(3 * gib)), Duration::from_secs(10));
    let first = book.close("wl_1");
    assert_eq!(first.counters.memory_kib_seconds, Some(2 * 10 * (1 << 20)), "1 GiB for 20 s");
    assert_eq!(first.peak, Some(3 * gib), "the high-water mark rose between readings");
    book.memory(("wl_1", "mem-1"), (Some(gib / 2), Some(3 * gib)), Duration::from_secs(10));
    let second = book.close("wl_1");
    assert_eq!(second.peak, Some(gib / 2), "a lifetime's mark that did not rise is not this peak");
    assert_eq!(book.close("wl_1").peak, None, "nothing read, no peak");
}
