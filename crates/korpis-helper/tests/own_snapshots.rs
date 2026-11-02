// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's own snapshots crossing to another node with it, between two btrfs filesystems made
//! for the test and mounted from loop devices: each is sent oldest first as an increment from the
//! one before, received beside the volume's pass, and becomes one of the volume's own snapshots
//! where the pass becomes the volume, still able to roll it back. It needs root, `mkfs.btrfs`,
//! and btrfs-progs, and is skipped without them.

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

const VOLUME: &str = "wl_m.data";
/// The replica volume the destination receives the volume's own snapshots into.
const OWN: &str = "own.wl_m.data";

/// Sends `snapshot` from `parent` out of `from` into the replica `into` of `to`, as the agents
/// carry it, and waits for the receiver to exit; what the receive came to.
fn carry(
    from: &mut Class,
    to: &mut Class,
    (snapshot, parent): (&str, &str),
    own: bool,
) -> Received {
    let into = if own { OWN } else { VOLUME };
    let read = from.send_replica(VOLUME, (snapshot, parent), own).unwrap();
    let (write, mut child) = to.receive_replica(into, snapshot).unwrap().expect("not yet held");
    let (mut r, mut w) = (std::fs::File::from(read), std::fs::File::from(write));
    std::io::copy(&mut r, &mut w).unwrap();
    drop(w);
    child.wait().unwrap();
    to.received(into, snapshot, &mut child).unwrap()
}

#[test]
fn a_volume_keeps_its_own_snapshots_on_the_node_it_moves_to() {
    if std::fs::metadata(korpis_helper::replica::BTRFS).is_err() {
        return;
    }
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let Some(a) = mounted(one.path(), true) else { return };
    let Some(b) = mounted(two.path(), true) else { return };
    let mut source = Class::open("local", &a.0.join("volumes")).unwrap();
    let mut destination = Class::open("local", &b.0.join("volumes")).unwrap();
    let r = helper::EnsureVolumeRequest {
        volume: VOLUME.into(),
        size_bytes: 64 << 20,
        ..Default::default()
    };
    let path = std::path::PathBuf::from(source.ensure(&r).unwrap().host_path);
    std::fs::write(path.join("one"), "first").unwrap();
    assert!(source.snapshot(VOLUME, "older").unwrap());
    std::fs::write(path.join("two"), "second").unwrap();
    assert!(source.snapshot(VOLUME, "newer").unwrap());
    std::fs::write(path.join("three"), "third").unwrap();

    let own = source.own_snapshots(VOLUME).unwrap();
    let names: Vec<_> = own.iter().map(|(n, p)| (n.as_str(), p.as_str())).collect();
    assert_eq!(names, [("older", ""), ("newer", "older")], "oldest first, each from the last");
    for (snapshot, parent) in &names {
        let came = carry(&mut source, &mut destination, (snapshot, parent), true);
        assert_eq!(came, Received::Held, "{snapshot} received");
    }
    assert_eq!(carry(&mut source, &mut destination, ("pass", ""), false), Received::Held);

    destination.promote_replica(VOLUME, "pass", "moved").unwrap();
    destination.adopt_snapshots(OWN, "moved").unwrap();
    destination.adopt_snapshots(OWN, "moved").unwrap();
    let kept = destination.own_snapshots("moved").unwrap();
    let names: Vec<_> = kept.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["older", "newer"], "the volume's own on its new node");
    assert!(destination.replica_snapshots(OWN).unwrap().is_empty(), "moved, not copied");

    destination.rollback("moved", "older").unwrap();
    let moved = destination.path_of("moved");
    assert_eq!(std::fs::read_to_string(moved.join("one")).unwrap(), "first");
    assert!(!moved.join("two").exists(), "rolled back to the older snapshot");
}
