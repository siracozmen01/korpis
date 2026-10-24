// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated cores (§5.2 of docs/05-scheduling.md): whole cores pinned to a workload are a limit
//! like any other, placed only on a node whose driver declares it holds them (Rule K-3), and no
//! driver of this release does.

use super::{Case, checks, driver, node};
use crate::schedule::Check;
use crate::workload::spec::resources::CpuIsolation;
use uuid::Uuid;

#[test]
fn dedicated_cores_are_placed_only_where_a_driver_declares_it_holds_them() {
    let mut pinned = node(2, &[]);
    pinned.drivers = vec![driver(&["memory", "cpu", "dedicated_cores"])];
    let mut case = Case::new(vec![node(1, &[]), pinned]);
    case.body.resources.cpu.isolation = CpuIsolation::DedicatedCores;
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Enforcement]);
    assert!(e.filtered[0].detail.contains("dedicated_cores"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
    case.body.resources.cpu.isolation = CpuIsolation::Shared;
    assert!(checks(&case.run(), 1).is_empty(), "shared time asks nothing of the driver");
}
