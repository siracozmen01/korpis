// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

/// Cores 1 to 4, two on each of two NUMA nodes.
fn cores() -> Vec<Core> {
    (1..=4).map(|id| Core { id, cpus: vec![id, id + 12], numa: u32::from(id > 2) }).collect()
}

#[test]
fn cores_come_from_the_numa_node_with_the_most_free_and_a_kept_core_first() {
    assert_eq!(choose(&cores(), &[], 2, &[]), Ok(vec![1, 2]), "one node, lowest ids");
    assert_eq!(choose(&cores(), &[1], 2, &[]), Ok(vec![3, 4]), "the node with both free");
    assert_eq!(choose(&cores(), &[], 1, &[4]), Ok(vec![4]), "kept where it is");
    assert_eq!(choose(&cores(), &[1, 2, 3], 2, &[]), Err("1 of 2 cores free".to_owned()));
}
