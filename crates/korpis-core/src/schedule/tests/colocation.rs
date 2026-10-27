// SPDX-License-Identifier: AGPL-3.0-or-later

//! What keeps a workload on one node: an existing volume it attaches (§7 of docs/06-storage.md),
//! a volume with replicas, which only a class sending increments holds (§2.2 there), and the
//! workloads it depends on, unless the mesh joins their nodes (§5 and §9.1 of
//! docs/07-networking.md).

use super::{GIB, PROJECT, body, node, req};
use crate::node::StorageClass;
use crate::node::driver::{Fencing, StorageBackend, StorageCaps, StorageFeature};
use crate::node::mesh::Pairs;
use crate::schedule::{Candidate, Check, Needed, Peer, Policies, Request, place};
use crate::tenancy::quota::{Bounds, Dimension};
use crate::workload::spec::volumes::{Replication, VolumeMode, VolumeSpec};
use crate::workload::volume::Home;
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
fn an_existing_volume_keeps_the_workload_on_the_node_holding_it() {
    let (one, two) = (node(1, &[]), node(2, &[]));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let held = [(Uuid::from_u128(77), Home::Node(two.id))];
    let request = Request { volumes: &held, ..req(&labels, &b) };
    let empty = Bounds::new();
    let nodes =
        [Candidate { node: &one, allocated: &empty }, Candidate { node: &two, allocated: &empty }];
    let e = place(&request, &nodes, &[], &Policies::default());
    assert_eq!(e.chosen, Some(two.id));
    assert!(e.filtered.iter().any(|r| r.node == one.id && r.check == Check::Volume));
}

/// A shared class on a node, of `store`.
fn shared(store: &str) -> StorageClass {
    StorageClass {
        name: "shared".into(),
        backend: StorageBackend::Network,
        capabilities: StorageCaps {
            features: std::collections::BTreeSet::new(),
            fencing: Fencing::LeaseOnly,
        },
        capacity_bytes: 0,
        free_bytes: 0,
        store: Some(store.into()),
        block: None,
    }
}

#[test]
fn a_volume_in_a_shared_store_is_held_by_every_node_offering_it() {
    let (mut one, mut two, three) = (node(1, &[]), node(2, &[]), node(3, &[]));
    one.storage_classes.push(shared("s1"));
    two.storage_classes.push(shared("s1"));
    let mut elsewhere = three.clone();
    elsewhere.storage_classes.push(shared("s2"));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let held = [(Uuid::from_u128(77), Home::Store("s1".into()))];
    let request = Request { volumes: &held, ..req(&labels, &b) };
    let empty = Bounds::new();
    let nodes = [&one, &two, &three, &elsewhere].map(|n| Candidate { node: n, allocated: &empty });
    let e = place(&request, &nodes, &[], &Policies::default());
    assert!(e.chosen == Some(one.id) || e.chosen == Some(two.id), "{:?}", e.chosen);
    let refused: Vec<_> =
        e.filtered.iter().filter(|r| r.check == Check::Volume).map(|r| r.node).collect();
    assert_eq!(refused, [three.id, elsewhere.id], "no class of it, or another store's");
}

#[test]
fn a_dependent_goes_to_its_targets_node_and_a_required_unplaced_one_nowhere() {
    let (one, two) = (node(1, &[]), node(2, &[]));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let db = Uuid::from_u128(77);
    let peers = [Peer { workload: db, project: PROJECT, node: two.id, labels: BTreeMap::new() }];
    let empty = Bounds::new();
    let nodes =
        [Candidate { node: &one, allocated: &empty }, Candidate { node: &two, allocated: &empty }];
    let needed = |required, targets| [Needed { name: "db".into(), required, targets }];
    let placed = |deps: &[Needed], peers: &[Peer]| {
        let request = Request { dependencies: deps, ..req(&labels, &b) };
        place(&request, &nodes, peers, &Policies::default())
    };
    let e = placed(&needed(true, vec![db]), &peers);
    assert_eq!(e.chosen, Some(two.id), "beside its database");
    assert!(e.filtered.iter().any(|r| r.node == one.id && r.check == Check::Dependency));
    let nowhere = placed(&needed(true, vec![db]), &[]);
    assert_eq!(nowhere.chosen, None, "a required dependency not placed holds it back");
    assert!(placed(&needed(false, vec![db]), &[]).chosen.is_some(), "an optional one does not");
    let split = [
        peers[0].clone(),
        Peer { workload: Uuid::from_u128(78), node: one.id, ..peers[0].clone() },
    ];
    let apart = placed(&needed(false, vec![db, Uuid::from_u128(78)]), &split);
    assert_eq!(apart.chosen, None, "no node reaches targets on two");
}

#[test]
fn a_dependent_goes_where_the_mesh_reaches_each_target_s_node() {
    let (one, two, three) = (node(1, &[]), node(2, &[]), node(3, &[]));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let (db, cache) = (Uuid::from_u128(77), Uuid::from_u128(78));
    let peers = [
        Peer { workload: db, project: PROJECT, node: two.id, labels: BTreeMap::new() },
        Peer { workload: cache, project: PROJECT, node: three.id, labels: BTreeMap::new() },
    ];
    let empty = Bounds::new();
    let nodes = [&one, &two, &three].map(|n| Candidate { node: n, allocated: &empty });
    let needed = [Needed { name: "db".into(), required: true, targets: vec![db, cache] }];
    let placed = |mesh: &Pairs| {
        let request = Request { dependencies: &needed, mesh, ..req(&labels, &b) };
        place(&request, &nodes, &peers, &Policies::default())
    };
    let joined: Pairs = [(one.id, two.id), (one.id, three.id)].into_iter().collect();
    let e = placed(&joined);
    assert_eq!(e.chosen, Some(one.id), "the one node that reaches both targets' nodes");
    let refused = |n: &crate::node::Node| {
        e.filtered.iter().any(|r| r.node == n.id && r.check == Check::Dependency)
    };
    assert!(refused(&two) && refused(&three), "neither reaches the other's");
    let half: Pairs = [(one.id, two.id)].into_iter().collect();
    assert_eq!(placed(&half).chosen, None, "no node reaches both");
}

/// A class named `local` on a node's own disk, able to send increments when `sends`.
fn local(sends: bool) -> StorageClass {
    let mut features = std::collections::BTreeSet::new();
    if sends {
        features.insert(StorageFeature::IncrementalSend);
    }
    StorageClass {
        name: "local".into(),
        backend: if sends { StorageBackend::Btrfs } else { StorageBackend::Directory },
        capabilities: StorageCaps { features, fencing: Fencing::None },
        capacity_bytes: 100 * GIB,
        free_bytes: 100 * GIB,
        store: None,
        block: None,
    }
}

#[test]
fn a_volume_with_replicas_goes_only_where_its_class_sends_increments() {
    let (mut one, mut two) = (node(1, &[]), node(2, &[]));
    for (n, sends) in [(&mut one, false), (&mut two, true)] {
        n.storage_classes.push(local(sends));
        n.allocatable.insert(Dimension::Disk, 100 * GIB);
        n.capacity.insert(Dimension::Disk, 100 * GIB);
    }
    let labels = BTreeMap::new();
    let mut b = body(GIB);
    b.volumes = vec![VolumeSpec {
        name: "world".into(),
        path: "/data".into(),
        size_bytes: GIB,
        storage_class: String::new(),
        mode: VolumeMode::ReadWrite,
        existing: None,
        from_backup: None,
        replication: Some(Replication { replicas: 1, interval: Replication::DEFAULT_INTERVAL }),
    }];
    let empty = Bounds::new();
    let nodes =
        [Candidate { node: &one, allocated: &empty }, Candidate { node: &two, allocated: &empty }];
    let e = place(&req(&labels, &b), &nodes, &[], &Policies::default());
    assert_eq!(e.chosen, Some(two.id), "the node's default class, btrfs, sends them");
    assert!(e.filtered.iter().any(|r| r.node == one.id && r.check == Check::StorageClass));
    b.volumes[0].replication = None;
    let e = place(&req(&labels, &b), &nodes, &[], &Policies::default());
    assert!(e.filtered.is_empty(), "with no replicas either will do");
}
