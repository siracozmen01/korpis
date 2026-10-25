// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

fn core(id: u32, cpus: &[u32]) -> pb::Core {
    pb::Core { id, cpus: cpus.to_vec(), ..Default::default() }
}

#[test]
fn the_cores_given_are_the_nodes_own_and_one_it_does_not_offer_is_refused() {
    let node = [core(1, &[1, 9]), core(2, &[2, 10])];
    let given = offered(&node, &[2]).unwrap();
    assert_eq!(given, [core(2, &[2, 10])], "its CPUs from the node's topology, not the request");
    assert!(offered(&node, &[1, 3]).unwrap_err().contains("core 3"));
    assert!(offered(&node, &[]).unwrap().is_empty());
}

#[test]
fn the_cgroup_parents_are_each_declared_once_and_never_empty() {
    let driver = |name: &str, parent: &str| pb::DriverInfo {
        name: name.into(),
        capabilities: pb::Capabilities { cgroup_parent: parent.into(), ..Default::default() }
            .into(),
        ..Default::default()
    };
    let drivers = [driver("oci", "/korpis"), driver("other", "/korpis"), driver("memory", "")];
    let kinds = super::super::DriverKinds::of(&drivers);
    assert_eq!(kinds.cgroup_parents().into_iter().collect::<Vec<_>>(), ["/korpis"]);
    assert_eq!(kinds.cgroup_parent("oci").as_deref(), Some("/korpis"));
    assert_eq!(kinds.cgroup_parent("memory"), None, "it declares none");
}
