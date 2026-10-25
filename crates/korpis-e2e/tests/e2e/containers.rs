// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a test's containers leave behind, removed however the test ends: every container the
//! node's `oci` driver made beneath its own cgroup parent, found by its cgroup, whose name is the
//! container's, and every container of a workload a test guards, found by its label.

use korpis_driver_kit::cores::{CGROUP_ROOT, Cores, Host};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where the `oci` driver of the node named `name` makes its containers: a cgroup parent of its
/// own, beneath which no other test's are, and any dedicated cores shared with the host, since
/// this machine's other services keep every CPU (§4 of docs/20-roadmap.md). Numbered within the
/// process: the tests of one process run at once and many name their node alike, and a parent
/// two nodes shared had each node's end remove the other's containers, killing them mid-test.
pub fn own(name: &str) -> Cores {
    static NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Cores {
        system: PathBuf::from("/sys/devices/system"),
        root: PathBuf::from(CGROUP_ROOT),
        parent: format!("/korpis-e2e-{}-{n}-{name}-oci", std::process::id()),
        host: Host::Shared,
    }
}

/// `ctr` in the `korpis` namespace.
fn ctr(args: &[&str]) -> Option<String> {
    let out = Command::new("ctr").args(["-n", "korpis"]).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether containerd holds a task for container `id`.
fn has_task(id: &str) -> bool {
    ctr(&["tasks", "ls", "-q"]).is_some_and(|ids| ids.lines().any(|t| t.trim() == id))
}

/// Removes container `id`: its task killed and removed, then it and its snapshot. The task goes
/// first and is waited for, however slow containerd is: a container removed while its task
/// remains leaves the task's shim running with no container to end it by.
fn remove(id: &str) {
    let _ = ctr(&["tasks", "kill", "-s", "SIGKILL", id]);
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while has_task(id) && std::time::Instant::now() < until {
        let _ = ctr(&["tasks", "rm", "-f", id]);
        std::thread::sleep(Duration::from_millis(200));
    }
    if has_task(id) {
        return;
    }
    let _ = ctr(&["containers", "rm", id]);
    let _ = ctr(&["snapshots", "rm", id]);
}

/// Removes every container whose cgroup is beneath the parent cgroup at `at`, then the cgroups.
pub fn remove_under(at: &Path) {
    for dir in korpis_driver_kit::cores::objects_under(at) {
        if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
            remove(id);
        }
    }
    korpis_driver_kit::cores::remove_under(at);
}

/// Removes every container of `workload`, by its label.
pub fn remove_workload(workload: &str) {
    let filter = format!("labels.\"korpis.workload\"=={workload}");
    let ids = ctr(&["containers", "ls", "-q", &filter]).unwrap_or_default();
    for id in ids.split_whitespace() {
        remove(id);
    }
}
