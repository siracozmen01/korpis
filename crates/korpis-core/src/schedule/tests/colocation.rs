// SPDX-License-Identifier: AGPL-3.0-or-later

//! What keeps a workload on one node: an existing volume it attaches (§7 of docs/06-storage.md),
//! and, until the overlay exists, the workloads it depends on (§5 of docs/07-networking.md).

use super::{GIB, PROJECT, body, node, req};
use crate::schedule::{Candidate, Check, Needed, Peer, Policies, Request, place};
use crate::tenancy::quota::Bounds;
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
fn an_existing_volume_keeps_the_workload_on_the_node_holding_it() {
    let (one, two) = (node(1, &[]), node(2, &[]));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let held = [(Uuid::from_u128(77), two.id)];
    let request = Request { volumes: &held, ..req(&labels, &b) };
    let empty = Bounds::new();
    let nodes =
        [Candidate { node: &one, allocated: &empty }, Candidate { node: &two, allocated: &empty }];
    let e = place(&request, &nodes, &[], &Policies::default());
    assert_eq!(e.chosen, Some(two.id));
    assert!(e.filtered.iter().any(|r| r.node == one.id && r.check == Check::Volume));
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
