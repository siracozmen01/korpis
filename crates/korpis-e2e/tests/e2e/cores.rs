// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated cores on a real node (§5.2 of docs/05-scheduling.md, §4.2 of docs/04-runtimes.md),
//! end to end: declared, placed, held, and freed. The agent offers the machine's last core, the
//! helper keeps the cpusets beneath the node's own cgroup parent, and its `oci` driver shares the
//! core with the host (`--dedicated-cores-host shared`), as this machine's other services need
//! every CPU, so no partition is made here. The workload asking for a core runs on its CPUs
//! alone, its node's `shared` cgroup leaves them while it is held, a second asking for one is not
//! placed while the first holds it and is once the first is retired, and the `shared` cgroup is
//! on every CPU again once none is held.

use crate::devices::{printed, unplaced};
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push};
use crate::support::{ControlPlane, converged};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::time::Duration;

/// The machine's last core, which every test here holding a core is given. The kernel lets a CPU
/// be exclusive to one cgroup among siblings at a time, so a test writing it while another holds
/// it is refused (`EINVAL`); these tests hold it one at a time.
pub(crate) static LAST_CORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A workload that says the CPUs it may run on, as the kernel lists them, and ends when asked.
fn pinned() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: pinned, version: "{VERSION}", description: says its CPUs, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        echo "cpus $(grep Cpus_allowed_list /proc/self/status | cut -f2)";
        trap 'exit 0' TERM; sleep 100000 & wait
preset:
  lifecycle: persistent
  console: logs
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu:    {{min: 50m, recommended: 100m}}
"#
    )
}

/// One dedicated core: 1,000 millicores reserved and its limit, whole.
fn asking() -> pb::IntentBody {
    let cpu = pb::CpuSpec {
        reservation_millis: 1000,
        limit_millis: 1000,
        isolation: pb::CpuIsolation::CPU_ISOLATION_DEDICATED_CORES.into(),
        ..Default::default()
    };
    let resources = pb::ResourceSpec { cpu: cpu.into(), ..Default::default() };
    pb::IntentBody { resources: resources.into(), ..Default::default() }
}

/// The CPUs the `shared` cgroup beneath `node`'s driver's cgroup parent may run on.
fn shared(node: &Node) -> BTreeSet<u32> {
    let at = node.containers.join("shared");
    let read = std::fs::read_to_string(at.join("cpuset.cpus.effective")).unwrap();
    korpis_topology::cpu_list(&read).unwrap()
}

/// Waits until `held` holds of the `shared` cgroup's CPUs beneath `node`'s parent.
async fn until_shared(node: &Node, held: impl Fn(&BTreeSet<u32>) -> bool) {
    for _ in 0..300 {
        if held(&shared(node)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the shared cgroup stayed on CPUs {:?}", shared(node));
}

#[tokio::test]
async fn a_workload_holding_a_core_runs_on_its_cpus_alone_and_frees_them_when_retired() {
    if !live() {
        return;
    }
    let _core = LAST_CORE.lock().await;
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let name = "cores-1";
    let Some(node) = Node::join_holding_cores(&cp, name).await else { return };
    let cpus: BTreeSet<u32> = node.cores[0].cpus.iter().copied().collect();
    let said = vec![format!("cpus {}", korpis_topology::listed(&cpus))];
    let agent = node.agent().await;
    let repo = push("pinned", pinned()).await;
    let tenant = cp.token_for(&["workload.read", "workload.console.read"]).await;

    let first = cp.declare_body(&project, "first", &repo, asking()).await;
    node.guard(&first);
    converged(&cp, &first).await;
    assert_eq!(printed(&cp, &tenant, &first).await, said, "its core's CPUs, and no other");
    until_shared(&node, |s| s.is_disjoint(&cpus)).await;

    let second = cp.declare_body(&project, "second", &repo, asking()).await;
    node.guard(&second);
    let why = unplaced(&cp, &second).await;
    let cores = why.filtered.iter().find(|f| f.check == "cores");
    assert!(cores.is_some_and(|f| f.detail.contains("0 of 1 cores free")), "{why:?}");

    cp.retire(&first).await;
    converged(&cp, &second).await;
    assert_eq!(printed(&cp, &tenant, &second).await, said, "freed, and held again");
    cp.retire(&second).await;
    until_shared(&node, |s| cpus.is_subset(s)).await;
    agent.stop().await;
}
