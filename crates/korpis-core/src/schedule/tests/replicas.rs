// SPDX-License-Identifier: AGPL-3.0-or-later

//! Choosing a volume's replicas (§2.2 of docs/06-storage.md): another node whose class sends
//! increments and has room, in another failure domain first, then the most room; and a replica
//! goes stale ten intervals after it last received.

use super::{GIB, node};
use crate::node::StorageClass;
use crate::node::driver::{Fencing, StorageBackend, StorageCaps, StorageFeature};
use crate::workload::replica::{Replica, Tending, Wanted, choose, tend};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A class named `local` with `free` bytes, able to send increments when `sends`.
fn local(sends: bool, free: u64) -> StorageClass {
    let features = if sends {
        std::collections::BTreeSet::from([StorageFeature::IncrementalSend])
    } else {
        std::collections::BTreeSet::new()
    };
    StorageClass {
        name: "local".into(),
        backend: if sends { StorageBackend::Btrfs } else { StorageBackend::Directory },
        capabilities: StorageCaps { features, fencing: Fencing::None },
        capacity_bytes: free,
        free_bytes: free,
        store: None,
        block: None,
    }
}

#[test]
fn a_replica_goes_to_another_rack_before_more_room() {
    let mut on = node(1, &[("rack", "r1")]);
    on.storage_classes.push(local(true, 100 * GIB));
    let mut same_rack = node(2, &[("rack", "r1")]);
    same_rack.storage_classes.push(local(true, 900 * GIB));
    let mut other_rack = node(3, &[("rack", "r2")]);
    other_rack.storage_classes.push(local(true, 50 * GIB));
    let mut no_send = node(4, &[("rack", "r3")]);
    no_send.storage_classes.push(local(false, 900 * GIB));
    let mut too_small = node(5, &[("rack", "r4")]);
    too_small.storage_classes.push(local(true, GIB / 2));
    let domains = ["rack".to_owned(), "region".to_owned()];
    let w = Wanted { on: &on, class: "", size: GIB, domains: &domains };
    let candidates = [&on, &same_rack, &other_rack, &no_send, &too_small];
    assert_eq!(choose(&w, &candidates, &[], 1), [other_rack.id], "another rack first");
    assert_eq!(choose(&w, &candidates, &[], 3), [other_rack.id, same_rack.id], "no more able");
    assert_eq!(choose(&w, &candidates, &[&other_rack], 1), [same_rack.id], "besides those kept");
    let none: [String; 0] = [];
    let plain = Wanted { domains: &none, ..w };
    assert_eq!(choose(&plain, &candidates, &[], 1), [same_rack.id], "with no domain, room");
}

#[test]
fn a_replica_is_stale_ten_intervals_after_it_last_received() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let minute = Duration::from_secs(60);
    let mut r = Replica {
        volume: Uuid::now_v7(),
        node: Uuid::now_v7(),
        chosen_at: t0,
        held: None,
        sending: None,
        stale_at: None,
    };
    assert!(!r.goes_stale(minute, t0 + 9 * minute));
    assert!(r.goes_stale(minute, t0 + 10 * minute), "never received since it was chosen");
    assert!(r.due(minute, t0), "holding nothing, at once");
    r.sending = Some((Uuid::now_v7(), t0));
    assert!(!r.due(minute, t0 + 59 * minute), "while a transfer is under way");
    assert!(r.due(minute, t0 + 60 * minute), "once its ticket expired");
    r.sending = None;
    r.held = Some((Uuid::now_v7(), t0 + 5 * minute));
    assert!(!r.goes_stale(minute, t0 + 10 * minute), "counted from what it received");
    assert_eq!(r.age(t0 + 7 * minute), Some(2 * minute));
    r.stale_at = Some(t0 + 15 * minute);
    assert!(!r.goes_stale(minute, t0 + 20 * minute), "marked once");
}

#[test]
fn a_replica_s_bytes_count_against_quota_and_not_on_the_volume_s_own_node() {
    use crate::tenancy::quota::{Dimension, Held};
    use crate::workload::spec::volumes::{Replication, VolumeMode, VolumeSpec};
    let mut b = super::body(GIB);
    b.volumes = vec![VolumeSpec {
        name: "world".into(),
        path: "/data".into(),
        size_bytes: 10 * GIB,
        storage_class: String::new(),
        mode: VolumeMode::ReadWrite,
        existing: None,
        from_backup: None,
        replication: Some(Replication { replicas: 2, interval: Replication::DEFAULT_INTERVAL }),
    }];
    let disk = |bounds: &crate::tenancy::quota::Bounds| bounds.get(&Dimension::Disk).copied();
    let here = crate::schedule::reservation(&b, false);
    let held = Held::of(&b).reservations.get(&Dimension::Disk).copied().unwrap_or(0);
    assert_eq!(held - disk(&here).unwrap_or(0), 20 * GIB, "two replicas held elsewhere");
}

/// A replica on `node` of a volume, chosen at `at`.
fn kept(node: Uuid, at: SystemTime, held: Option<SystemTime>) -> Replica {
    Replica {
        volume: Uuid::nil(),
        node,
        chosen_at: at,
        held: held.map(|h| (Uuid::now_v7(), h)),
        sending: None,
        stale_at: None,
    }
}

#[test]
fn a_pass_drops_what_cannot_stay_keeps_the_newest_and_chooses_the_rest() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let minute = Duration::from_secs(60);
    let mut on = node(1, &[("rack", "r1")]);
    on.storage_classes.push(local(true, 100 * GIB));
    let mut full = node(2, &[("rack", "r2")]);
    full.storage_classes.push(local(true, 0));
    let mut fresh = node(3, &[("rack", "r3")]);
    fresh.storage_classes.push(local(true, 900 * GIB));
    let mut spare = node(4, &[("rack", "r4")]);
    spare.storage_classes.push(local(true, 900 * GIB));
    let mut no_send = node(5, &[("rack", "r5")]);
    no_send.storage_classes.push(local(false, 900 * GIB));
    let domains = ["rack".to_owned()];
    let w = Wanted { on: &on, class: "", size: GIB, domains: &domains };
    let serving = [&on, &full, &fresh, &spare, &no_send];
    let ready = [&on, &fresh, &spare, &no_send];
    let gone = Uuid::now_v7();
    let replicas = [
        kept(full.id, t0, None),
        kept(fresh.id, t0, Some(t0 + 3 * minute)),
        kept(no_send.id, t0, Some(t0 + 4 * minute)),
        kept(gone, t0, Some(t0 + 4 * minute)),
    ];
    let now = t0 + 12 * minute;
    let two = tend(&w, Some((2, minute)), &replicas, (&serving, &ready), now);
    assert_eq!(
        two,
        Tending {
            dropped: vec![no_send.id, gone],
            stale: vec![full.id],
            chosen: vec![],
            due: vec![fresh.id, full.id],
        },
        "a full node still keeps what it holds, and goes stale having received nothing"
    );
    let one = tend(&w, Some((1, minute)), &replicas, (&serving, &ready), now);
    assert_eq!(one.dropped, [no_send.id, gone, full.id], "the one holding the newest stays");
    let three = tend(&w, Some((3, minute)), &replicas, (&serving, &ready), now);
    assert_eq!(three.chosen, [spare.id], "the rest chosen among the ready");
    let none = tend(&w, None, &replicas, (&serving, &ready), now);
    assert_eq!(none.dropped.len(), 4, "no replicas asked for");
}
