// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicas between two btrfs filesystems, each made for the test and mounted from a loop
//! device, standing for two nodes: a volume's snapshot is sent whole and received, then only
//! what changed is sent from it, the receiver holding both read-only; an older snapshot is
//! released on both sides; the newest becomes the volume where it was received, holding what
//! the last increment held; a stream that ends short leaves nothing behind. It needs root,
//! `mkfs.btrfs`, and btrfs-progs, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

#[allow(dead_code, reason = "each test binary uses part of the shared fixture")]
mod fixture;

use fixture::mounted;
use korpis_helper::replica::Received;
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use std::time::Duration;

const VOLUME: &str = "wl_r.data";

/// Sends snapshot `snapshot` of the volume from `from` to `to`, from `parent`, through the two
/// pipes as the agents would carry it; what the receive came to.
fn carry(from: &mut Class, to: &mut Class, snapshot: &str, parent: &str) -> Received {
    let read = from.send_replica(VOLUME, snapshot, parent).unwrap();
    let (write, mut child) = to.receive_replica(VOLUME, snapshot).unwrap().expect("not yet held");
    let (mut r, mut w) = (std::fs::File::from(read), std::fs::File::from(write));
    std::io::copy(&mut r, &mut w).unwrap();
    drop(w);
    for _ in 0..600 {
        match to.received(VOLUME, snapshot, &mut child).unwrap() {
            Received::Running => std::thread::sleep(Duration::from_millis(50)),
            came => return came,
        }
    }
    panic!("the receive never ended");
}

#[test]
fn a_volume_is_sent_whole_then_by_increments_and_its_replica_becomes_the_volume() {
    if std::fs::metadata(korpis_helper::replica::BTRFS).is_err() {
        return;
    }
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let Some(a) = mounted(one.path(), true) else { return };
    let Some(b) = mounted(two.path(), true) else { return };
    let mut sender = Class::open("local", &a.0.join("volumes")).unwrap();
    let mut receiver = Class::open("local", &b.0.join("volumes")).unwrap();
    let r = helper::EnsureVolumeRequest {
        volume: VOLUME.into(),
        size_bytes: 64 << 20,
        ..Default::default()
    };
    let path = std::path::PathBuf::from(sender.ensure(&r).unwrap().host_path);
    std::fs::write(path.join("one"), "first").unwrap();

    assert_eq!(carry(&mut sender, &mut receiver, "s1", ""), Received::Held, "sent whole");
    std::fs::write(path.join("two"), "second").unwrap();
    assert_eq!(carry(&mut sender, &mut receiver, "s2", "s1"), Received::Held, "an increment");
    assert_eq!(receiver.replica_snapshots(VOLUME).unwrap(), ["s1", "s2"]);
    assert!(receiver.receive_replica(VOLUME, "s2").unwrap().is_none(), "held already");

    assert!(sender.release_replica(VOLUME, "s1").unwrap());
    assert!(receiver.release_replica(VOLUME, "s1").unwrap());
    assert_eq!(sender.replica_snapshots(VOLUME).unwrap(), ["s2"], "the newest kept to send from");

    let short = receiver.receive_replica(VOLUME, "s3").unwrap().expect("not held");
    let (write, mut child) = short;
    drop(write);
    let came = loop {
        match receiver.received(VOLUME, "s3", &mut child).unwrap() {
            Received::Running => std::thread::sleep(Duration::from_millis(50)),
            came => break came,
        }
    };
    assert!(matches!(came, Received::Failed(_)), "{came:?}");
    assert_eq!(receiver.replica_snapshots(VOLUME).unwrap(), ["s2"], "nothing of it is kept");

    let promoted = receiver.promote_replica(VOLUME, "s2").unwrap();
    assert_eq!(std::fs::read_to_string(promoted.join("one")).unwrap(), "first");
    assert_eq!(std::fs::read_to_string(promoted.join("two")).unwrap(), "second");
    std::fs::write(promoted.join("three"), "writable").unwrap();
    assert!(receiver.promote_replica(VOLUME, "s2").is_err(), "the volume is there now");
}
