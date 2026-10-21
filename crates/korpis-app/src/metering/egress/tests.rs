// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::quota::EgressBudget;
use std::time::Duration;

const ORG: Uuid = Uuid::from_u128(1);
const P1: Uuid = Uuid::from_u128(11);
const P2: Uuid = Uuid::from_u128(12);
const A: Uuid = Uuid::from_u128(101);
const B: Uuid = Uuid::from_u128(102);

fn entry(id: u128, owner: QuotaOwner, bytes: u64, exhausted: Exhaustion) -> QuotaEntry {
    QuotaEntry {
        id: Uuid::from_u128(id),
        owner,
        selector: None,
        nodes: None,
        reservations: BTreeMap::new(),
        limits: BTreeMap::new(),
        version: 1,
        egress: Some(EgressBudget { bytes_per_month: bytes, exhausted, spent: None }),
        held: None,
    }
}

fn at(workload: Uuid, project: Uuid, (node, zone): (u128, &str), bytes: u64) -> EgressAt {
    EgressAt {
        workload,
        project,
        organization: ORG,
        labels: BTreeMap::new(),
        node: Some(Uuid::from_u128(node)),
        node_labels: BTreeMap::from([("zone".to_owned(), zone.to_owned())]),
        bytes,
        placed: zone == "eu",
    }
}

#[test]
fn a_spent_entry_holds_what_it_counts_where_placed_to_the_strictest_exhaustion() {
    // A sent from a node it has left, and is placed in the EU now.
    let uses = [at(A, P1, (1, "eu"), 600), at(A, P1, (2, "us"), 500), at(B, P2, (1, "eu"), 100)];
    let floor = Exhaustion::Floor(5_000_000);
    let mut in_the_eu = entry(3, QuotaOwner::Organization(ORG), 1_000, floor);
    in_the_eu.nodes = Some(LabelSelector {
        match_labels: [("zone".to_owned(), "eu".to_owned())].into(),
        expressions: Vec::new(),
    });
    let entries = [
        entry(1, QuotaOwner::Organization(ORG), 1_000, floor),
        entry(2, QuotaOwner::Project(P1), 500, Exhaustion::CutOff),
        in_the_eu,
        entry(4, QuotaOwner::Project(P2), 10_000, Exhaustion::CutOff),
    ];
    let chains = BTreeMap::from([(ORG, vec![ORG])]);
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_812_800);
    let r = reckon(&entries, &uses, &chains, (t, t));
    let spent: Vec<(u64, bool)> = r.spent.iter().map(|(_, s)| (s.bytes, s.exhausted)).collect();
    assert_eq!(
        spent,
        [(1_200, true), (1_100, true), (700, false), (100, false)],
        "the organization's, the project's, the EU nodes', and the other project's"
    );
    assert_eq!(r.held.get(&A), Some(&Exhaustion::CutOff), "a cut-off before any floor");
    assert_eq!(r.held.get(&B), Some(&floor), "counted by the organization's spent entry");
    assert_eq!(r.held.len(), 2);
}
