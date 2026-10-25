// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

/// A `system` tree of `cores` cores of two threads each, CPU `n` and `n + cores` siblings, on
/// two NUMA nodes split down the middle, with `online` the online list.
fn system(cores: u32, online: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let put = |path: &str, text: &str| {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    put("cpu/online", online);
    for n in 0..cores {
        let siblings = format!("{n},{}\n", n + cores);
        put(&format!("cpu/cpu{n}/topology/core_cpus_list"), &siblings);
        put(&format!("cpu/cpu{}/topology/core_cpus_list", n + cores), &siblings);
    }
    let half = cores / 2;
    put("node/node0/cpulist", &format!("0-{},{}-{}", half - 1, cores, cores + half - 1));
    put("node/node1/cpulist", &format!("{half}-{},{}-{}", cores - 1, cores + half, 2 * cores - 1));
    dir
}

#[test]
fn every_core_but_cpu_0_s_is_offered_whole_with_its_numa_node() {
    let dir = system(4, "0-7\n");
    let read = cores(dir.path(), None);
    let ids: Vec<(u32, Vec<u32>, u32)> =
        read.iter().map(|c| (c.id, c.cpus.clone(), c.numa)).collect();
    assert_eq!(ids, [(1, vec![1, 5], 0), (2, vec![2, 6], 1), (3, vec![3, 7], 1)]);
}

#[test]
fn a_core_with_a_sibling_offline_or_not_named_is_not_offered() {
    let dir = system(4, "0-6\n");
    let ids: Vec<u32> = cores(dir.path(), None).iter().map(|c| c.id).collect();
    assert_eq!(ids, [1, 2], "core 3's sibling 7 is offline");
    let named = cpu_list("2-3").unwrap();
    let ids: Vec<u32> = cores(dir.path(), Some(&named)).iter().map(|c| c.id).collect();
    assert_eq!(ids, [2]);
    assert!(cores(&dir.path().join("missing"), None).is_empty(), "no topology, no cores");
}

#[test]
fn a_cpu_list_is_read_as_the_kernel_writes_it() {
    assert_eq!(
        cpu_list("0-2,5,7-8\n").unwrap().into_iter().collect::<Vec<_>>(),
        [0, 1, 2, 5, 7, 8]
    );
    assert!(cpu_list("3-1").is_err());
    assert!(cpu_list("a").is_err());
    assert!(cpu_list("0-9000").is_err());
}
