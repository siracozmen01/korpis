// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

fn put(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Four cores of two threads, CPU `n` and `n + 4` siblings, on one NUMA node, and a cgroup root.
fn cores(dir: &Path, host: Host) -> Cores {
    let system = dir.join("system");
    put(&system, "cpu/online", "0-7\n");
    for n in 0..4 {
        for cpu in [n, n + 4] {
            put(
                &system,
                &format!("cpu/cpu{cpu}/topology/core_cpus_list"),
                &format!("{n},{}", n + 4),
            );
        }
    }
    put(&system, "node/node0/cpulist", "0-7\n");
    std::fs::create_dir_all(dir.join("cgroup/korpis")).unwrap();
    Cores { system, root: dir.join("cgroup"), parent: "/korpis".into(), host }
}

fn core(id: u32, cpus: &[u32]) -> pb::Core {
    pb::Core { id, cpus: cpus.to_vec(), ..Default::default() }
}

#[test]
fn an_object_holding_cores_gets_their_cpuset_once_the_topology_agrees() {
    let dir = tempfile::tempdir().unwrap();
    let c = cores(dir.path(), Host::Kept);
    let set = c.given(&[core(3, &[3, 7]), core(1, &[1, 5])]).unwrap().unwrap();
    assert_eq!(set, Cpuset { cpus: "1,3,5,7".into(), mems: "0".into() });
    assert_eq!(c.given(&[]).unwrap(), None);
    assert!(c.given(&[core(0, &[0, 4])]).unwrap_err().contains("core 0"), "CPU 0's");
    assert!(c.given(&[core(1, &[1, 2])]).is_err(), "not that core's CPUs");
    assert_eq!(c.cgroup("w", true), "/korpis/dedicated/w");
    assert_eq!(c.cgroup("w", false), "/korpis/shared/w");
}

#[test]
fn an_objects_cgroup_is_found_where_it_was_made_an_old_one_included() {
    let dir = tempfile::tempdir().unwrap();
    let c = cores(dir.path(), Host::Kept);
    let at = dir.path().join("cgroup/korpis");
    assert_eq!(c.dir("new"), at.join("shared/new"), "not made yet");
    for (id, place) in [("held", "dedicated/held"), ("old", "old"), ("plain", "shared/plain")] {
        std::fs::create_dir_all(at.join(place)).unwrap();
        assert_eq!(c.dir(id), at.join(place));
    }
}

#[test]
fn a_holding_cgroup_is_made_a_partition_unless_the_host_shares_its_cores() {
    let dir = tempfile::tempdir().unwrap();
    let held = dir.path().join("cgroup/korpis/dedicated/w");
    put(&held, "cpuset.cpus", "1,5\n");
    put(&held, "cpuset.cpus.partition", "member\n");
    cores(dir.path(), Host::Shared).partition(&held).unwrap();
    let read = |f: &str| std::fs::read_to_string(held.join(f)).unwrap();
    assert_eq!(read("cpuset.cpus.partition"), "member\n", "the host shares them");
    cores(dir.path(), Host::Kept).partition(&held).unwrap();
    assert_eq!(read("cpuset.cpus.exclusive"), "1,5");
    assert_eq!(read("cpuset.cpus.partition"), "root");
    let shared = dir.path().join("cgroup/korpis/shared/v");
    std::fs::create_dir_all(&shared).unwrap();
    cores(dir.path(), Host::Kept).partition(&shared).unwrap();
    assert!(!shared.join("cpuset.cpus.partition").exists(), "a shared object holds none");
}

#[test]
fn dedicated_cores_are_declared_held_only_where_nothing_runs_outside_and_the_kernel_can() {
    let dir = tempfile::tempdir().unwrap();
    let (kept, shared) = (cores(dir.path(), Host::Kept), cores(dir.path(), Host::Shared));
    let no = kept.declared();
    assert!(!no.enforced && no.granularity.contains("partition"), "{no:?}");
    let yes = shared.declared();
    assert!(yes.enforced && yes.granularity.contains("host"), "{yes:?}");
    put(&dir.path().join("cgroup"), "cgroup.subtree_control", "cpu cpuset memory");
    put(&dir.path().join("cgroup/system.slice"), "cpuset.cpus.exclusive", "");
    put(&dir.path().join("cgroup/system.slice"), "cpuset.cpus.partition", "member");
    assert!(kept.declared().enforced, "{:?}", kept.declared());
    std::fs::create_dir_all(dir.path().join("cgroup/korpis/old")).unwrap();
    let old = kept.declared();
    assert!(!old.enforced && old.granularity.contains("old runs outside"), "{old:?}");
}

#[test]
fn a_cgroup_parent_is_a_clean_path_below_the_root() {
    assert_eq!(parent("/korpis/").unwrap(), "/korpis");
    for bad in ["korpis", "/", "/korpis/../system.slice", ""] {
        assert!(parent(bad).is_err(), "{bad}");
    }
}
