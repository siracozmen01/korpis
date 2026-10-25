// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

/// A cgroup tree of the test's own, eight CPUs online, keeping `/korpis`, `/vm`, and `/a/b`.
fn cores(dir: &Path) -> Cores {
    std::fs::write(dir.join("online"), "0-7\n").unwrap();
    let parents = ["/korpis", "/vm", "/a/b"].map(PathBuf::from).to_vec();
    let config = CoresConfig { root: dir.join("cgroup"), online: dir.join("online"), parents };
    Cores::new(config).unwrap()
}

fn held(sets: &[(&str, &[u32])]) -> BTreeMap<String, BTreeSet<u32>> {
    sets.iter().map(|(p, c)| ((*p).to_owned(), c.iter().copied().collect())).collect()
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join("cgroup").join(file)).unwrap()
}

#[test]
fn the_shared_cgroups_leave_every_held_cpu_and_each_parent_is_made_exclusive_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (c, d) = (cores(dir.path()), dir.path());
    let shared = c.hold(&held(&[("/korpis", &[2, 3, 6]), ("/vm", &[4])])).unwrap();
    assert_eq!(shared, [0, 1, 5, 7]);
    for parent in ["korpis", "vm"] {
        assert_eq!(read(d, &format!("{parent}/shared/cpuset.cpus")), "0-1,5,7");
        assert_eq!(read(d, &format!("{parent}/cgroup.subtree_control")), "+cpuset");
    }
    // Siblings' exclusive CPUs never overlap, as the kernel requires.
    assert_eq!(read(d, "korpis/cpuset.cpus.exclusive"), "2-3,6");
    assert_eq!(read(d, "korpis/dedicated/cpuset.cpus.exclusive"), "2-3,6");
    assert_eq!(read(d, "vm/dedicated/cpuset.cpus.exclusive"), "4");
    c.hold(&held(&[("/korpis", &[]), ("/vm", &[4])])).unwrap();
    assert_eq!(read(d, "korpis/shared/cpuset.cpus"), "0-3,5-7", "freed, shared again");
    assert_eq!(read(d, "korpis/cpuset.cpus.exclusive"), "");
}

#[test]
fn every_cgroup_above_a_nested_parent_is_given_its_exclusive_cpus() {
    let dir = tempfile::tempdir().unwrap();
    let (c, d) = (cores(dir.path()), dir.path());
    c.hold(&held(&[("/a/b", &[1, 2])])).unwrap();
    assert_eq!(read(d, "a/cpuset.cpus.exclusive"), "1-2");
    assert_eq!(read(d, "a/b/cpuset.cpus.exclusive"), "1-2");
    assert_eq!(read(d, "a/b/dedicated/cpuset.cpus.exclusive"), "1-2");
    assert_eq!(read(d, "a/cgroup.subtree_control"), "+cpuset");
    assert_eq!(read(d, "a/b/shared/cpuset.cpus"), "0,3-7");
}

#[test]
fn a_cpu_not_online_held_twice_or_every_one_and_a_parent_not_kept_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let c = cores(dir.path());
    assert!(c.hold(&held(&[("/korpis", &[9])])).is_err(), "not online");
    assert!(c.hold(&held(&[("/korpis", &[2]), ("/vm", &[2])])).is_err(), "held twice");
    assert!(c.hold(&held(&[("/korpis", &[0, 1, 2, 3, 4, 5, 6, 7])])).is_err(), "none shared");
    assert!(c.hold(&held(&[("/system.slice", &[2])])).is_err(), "not kept");
    assert!(!dir.path().join("cgroup/system.slice").exists());
    for bad in ["korpis", "/", "/korpis/../system.slice"] {
        let parents = vec![bad.into()];
        let config = CoresConfig { root: "/r".into(), online: "/o".into(), parents };
        assert!(Cores::new(config).is_err(), "{bad}");
    }
}

#[test]
fn a_cpu_list_is_written_as_the_kernel_writes_one() {
    let set = |v: &[u32]| v.iter().copied().collect::<BTreeSet<u32>>();
    assert_eq!(listed(&set(&[1, 2, 3, 5, 7, 8])), "1-3,5,7-8");
    assert_eq!(listed(&set(&[])), "");
    assert_eq!(parse("1-3,5\n").unwrap(), set(&[1, 2, 3, 5]));
}
