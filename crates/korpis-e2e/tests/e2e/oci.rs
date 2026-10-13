// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Phase 0 sequence on a real node (§4 of docs/20-roadmap.md): a thin recipe is declared,
//! placed on the node, and converged, running in a hardened container with its configured value
//! and its volume; the agent restarts and adopts it without the workload starting again; every
//! step it took is an effect naming its grant; and its retirement removes the container and
//! keeps the volume.

use crate::node::{Node, live};
use crate::support::{ControlPlane, push_sleeper};
use korpis_agent::runtime::{Runtime as _, RuntimeError};
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Waits up to `secs` seconds until `pick` holds of the workload's status.
async fn status(
    cp: &ControlPlane,
    w: &str,
    secs: u64,
    pick: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    for _ in 0..secs * 5 {
        let s = cp.status(w).await;
        if pick(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} never reached the status awaited: {:?}", cp.status(w).await);
}

fn converged(s: &pb::WorkloadStatus) -> bool {
    s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED)
}

/// Waits until `sql` counts at least `n`.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..150 {
        if cp.scalar::<i64>(sql).await >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{sql} never reached {n}");
}

/// The workload's one volume on the node.
fn volume(node: &Node) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&node.volumes)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    dirs.remove(0)
}

/// The lines of `file`, once it has at least one.
async fn lines(file: &Path) -> Vec<String> {
    for _ in 0..100 {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if !text.trim().is_empty() {
            return text.lines().map(str::to_owned).collect();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} stayed empty", file.display());
}

#[tokio::test]
async fn a_thin_recipe_runs_survives_an_agent_restart_untouched_and_retires() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "fra-1").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "sleeper", &push_sleeper().await).await;
    node.guard(&w);

    // Placed, fetched by digest, and running in its container.
    let s = status(&cp, &w, 180, converged).await;
    let observed = s.observation.as_option().unwrap().clone();
    assert_eq!(observed.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_RUNNING));
    let vol = volume(&node);
    assert_eq!(lines(&vol.join("greeting")).await, ["hello"], "the configured value");
    assert_eq!(lines(&vol.join("starts")).await.len(), 1);
    let runtime = node.runtime();
    let pid = runtime.inspect(&observed.runtime_id).await.unwrap().pid;
    assert!(pid > 0);

    // The agent restarts; the workload does not.
    agent.stop().await;
    let agent = node.agent().await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'node.adopt'", 1).await;
    status(&cp, &w, 30, converged).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(lines(&vol.join("starts")).await.len(), 1, "never started again");
    assert_eq!(runtime.inspect(&observed.runtime_id).await.unwrap().pid, pid, "the same process");
    let sql = "SELECT count(*) FROM effect WHERE (action LIKE 'step.%' OR action = 'node.adopt') \
               AND origin_seq IS NOT NULL AND grant_id IS NULL";
    assert_eq!(cp.scalar::<i64>(sql).await, 0, "every node action names its grant");

    // Retired: the container goes, the volume stays.
    cp.retire(&w).await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'workload.retire'", 1).await;
    let gone = runtime.inspect(&observed.runtime_id).await;
    assert!(matches!(gone, Err(RuntimeError::NotFound(_))), "{gone:?}");
    assert!(vol.join("starts").exists(), "a volume outlives its workload");
    agent.stop().await;
}
