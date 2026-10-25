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

#[test]
fn what_a_placement_holds_is_integrated_whatever_runs_and_nothing_for_none() {
    use korpis_proto::korpis::v1 as pb;
    let mut book = Book::default();
    let gpu = [(pb::DeviceClass::DEVICE_CLASS_GPU, 1)];
    book.held("wl_1", (2, &gpu), Duration::from_secs(10));
    book.held("wl_1", (2, &gpu), Duration::from_secs(10));
    book.held("wl_2", (0, &[]), Duration::from_secs(10));
    let counters = book.close("wl_1").counters;
    assert_eq!(counters.dedicated_core_seconds, Some(40), "two cores for 20 s");
    assert_eq!(counters.device_seconds.len(), 1);
    assert_eq!(counters.device_seconds[0].seconds, 20, "one GPU for 20 s");
    let none = book.close("wl_2").counters;
    assert_eq!(none.dedicated_core_seconds, None, "absent, not zero, where nothing is held");
}

fn table(egress: u64, endpoints: &[(&str, u64)]) -> agent::MeterCounters {
    let endpoints = endpoints
        .iter()
        .map(|(name, received)| agent::EndpointCounters {
            endpoint: (*name).into(),
            received_bytes: *received,
            sent_bytes: received * 2,
            ..Default::default()
        })
        .collect();
    agent::MeterCounters { egress_bytes: Some(egress), endpoints, ..Default::default() }
}

fn received(c: &agent::MeterCounters, name: &str) -> Option<u64> {
    c.endpoints.iter().find(|e| e.endpoint == name).map(|e| e.received_bytes)
}

#[test]
fn an_endpoints_bytes_run_on_across_a_reset_and_are_kept_when_it_stops_being_counted() {
    let mut book = Book::default();
    book.read("wl_1", "table", table(1000, &[("game", 100), ("map", 7)]), t(0));
    book.read("wl_1", "table", table(1500, &[("game", 150), ("map", 9)]), t(10));
    // The network was made again, its counters with it.
    book.read("wl_1", "table", table(20, &[("game", 10), ("map", 1)]), t(20));
    let reset = book.pending("wl_1");
    assert_eq!(
        (reset.counters.egress_bytes, received(&reset.counters, "game")),
        (Some(1520), Some(160))
    );
    assert_eq!(reset.losses.len(), 1, "what was sent between the readings is lost");
    // The map's accounting is turned off: what it carried stays counted, and nothing is lost.
    book.read("wl_1", "table", table(40, &[("game", 12)]), t(30));
    let off = book.close("wl_1");
    assert_eq!(received(&off.counters, "map"), Some(10), "nine before the reset, one after");
    assert_eq!(received(&off.counters, "game"), Some(162));
    assert_eq!(off.counters.endpoints[1].sent_bytes, 20, "its bytes sent, kept with it");
    assert_eq!(off.losses.len(), 1);
}
