// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated cores (§5.2 of docs/05-scheduling.md): whole cores pinned to a workload are placed
//! only on a node whose driver declares it holds them (Rule K-3) and which offers enough free
//! cores, and the placement reserves every logical CPU of the cores it is given.

use super::{Case, checks, driver, node};
use crate::node::Node;
use crate::node::cores::Core;
use crate::schedule::Check;
use crate::workload::spec::resources::CpuIsolation;
use uuid::Uuid;

/// Node `n`, whose driver holds dedicated cores, offering cores 1 to 3, two threads each.
fn pinned(n: u128) -> Node {
    let mut node = node(n, &[]);
    node.drivers = vec![driver(&["memory", "cpu", "dedicated_cores"])];
    node.cores = (1..=3).map(|id| Core { id, cpus: vec![id, id + 12], numa: 0 }).collect();
    node
}

/// A case asking for `cores` dedicated cores.
fn asking(cores: u64, nodes: Vec<Node>) -> Case {
    let mut case = Case::new(nodes);
    case.body.resources.cpu.isolation = CpuIsolation::DedicatedCores;
    case.body.resources.cpu.reservation_millis = cores * 1_000;
    case.body.resources.cpu.limit_millis = cores * 1_000;
    case
}

#[test]
fn dedicated_cores_are_placed_only_where_a_driver_declares_it_holds_them() {
    let mut case = asking(1, vec![node(1, &[]), pinned(2)]);
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Enforcement, Check::Cores]);
    assert!(e.filtered[0].detail.contains("dedicated_cores"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
    case.body.resources.cpu.isolation = CpuIsolation::Shared;
    assert!(checks(&case.run(), 1).is_empty(), "shared time asks nothing of the driver");
}

#[test]
fn a_node_with_too_few_free_cores_is_passed_over_saying_how_many_are_free() {
    let mut case = asking(2, vec![pinned(2)]);
    case.held_cores.insert(Uuid::from_u128(2), vec![1, 3]);
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Cores]);
    assert!(e.filtered[0].detail.contains("1 of 2 cores free"), "{}", e.filtered[0].detail);
}

#[test]
fn a_held_core_reserves_its_siblings_so_three_cores_take_six_cpus() {
    // The node has 4,000 millicores: two cores of two threads fit, three do not.
    let e = asking(3, vec![pinned(2)]).run();
    assert_eq!(checks(&e, 2), [Check::Capacity], "{e:?}");
    assert!(e.filtered[0].detail.contains("needs 6000"), "{}", e.filtered[0].detail);
    assert_eq!(asking(2, vec![pinned(2)]).run().chosen, Some(Uuid::from_u128(2)));
}

#[test]
fn a_placement_holding_cores_reserves_their_threads_and_not_its_declared_cpu() {
    use crate::schedule::cores::reservation_on;
    use crate::tenancy::quota::Dimension;
    let node = pinned(2);
    let body = asking(2, Vec::new()).body;
    assert_eq!(reservation_on((&body, false), &node, &[1, 3]).get(&Dimension::Cpu), Some(&4_000));
    assert_eq!(reservation_on((&body, false), &node, &[]).get(&Dimension::Cpu), Some(&2_000));
}
