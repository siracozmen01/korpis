// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota that counts where workloads run (§5.1 of docs/05-scheduling.md, "Where a workload
//! runs"): a node is refused while what the workload would run there exceeds an entry counting
//! that node, the nodes it does not count are not, and a workload it already counts is refused
//! only for growth.

use super::{GIB, ORG, body, checks, node, req};
use crate::labels::LabelSelector;
use crate::schedule::{Candidate, Check, Counting, PlacementPolicy, place};
use crate::tenancy::quota::{Bounds, Dimension, Held, QuotaEntry, QuotaOwner, Totals};
use std::collections::BTreeMap;
use uuid::Uuid;

/// An entry bounding the memory reserved on the nodes labelled for the EU.
fn in_the_eu(memory: u64) -> QuotaEntry {
    let eu = BTreeMap::from([("region".to_owned(), "eu".to_owned())]);
    QuotaEntry {
        id: Uuid::from_u128(77),
        owner: QuotaOwner::Organization(ORG),
        selector: None,
        nodes: Some(LabelSelector { match_labels: eu, expressions: Vec::new() }),
        reservations: Bounds::from([(Dimension::Memory, memory)]),
        limits: Bounds::new(),
        version: 1,
        held: Some(Totals::default()),
    }
}

#[test]
fn a_node_an_entry_counts_is_refused_while_the_workload_would_exceed_it_there() {
    let (eu, us) = (node(1, &[("region", "eu")]), node(2, &[("region", "us")]));
    let empty = Bounds::new();
    let candidates = [&eu, &us].map(|n| Candidate { node: n, allocated: &empty });
    let (labels, b) = (BTreeMap::new(), body(2 * GIB));
    let entry = in_the_eu(3 * GIB);
    // Another workload runs 2 GiB in the EU.
    let other = Totals::sum([&Held::of(&body(2 * GIB))]);
    let quota = [Counting { entry: &entry, rest: other.clone(), counted: None }];
    let mut r = req(&labels, &b);
    r.quota = &quota;
    let e = place(&r, &candidates, &[], &PlacementPolicy::default());
    assert_eq!(checks(&e, 1), [Check::Quota]);
    assert!(e.filtered[0].detail.contains("memory_bytes"), "{}", e.filtered[0].detail);
    assert!(checks(&e, 2).is_empty(), "the entry does not count the US");
    assert_eq!(e.chosen, Some(us.id));

    let held = Held::of(&b);
    let quota = [Counting { entry: &entry, rest: other, counted: Some(&held) }];
    r.quota = &quota;
    let e = place(&r, &candidates, &[], &PlacementPolicy::default());
    assert!(checks(&e, 1).is_empty(), "what it already runs there is no growth");
}
