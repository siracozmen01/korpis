// SPDX-License-Identifier: AGPL-3.0-or-later

//! Snapshots on a real node (§4 of docs/06-storage.md), its volumes on a btrfs made for the test:
//! a snapshot taken while the workload runs, the volume griefed, then rolled back by a plan that
//! stops the workload, swaps the snapshot's data in, and starts it again, so what was written
//! since is gone and the workload runs on what the snapshot held; and the snapshot deleted. It
//! needs root, containerd, and `mkfs.btrfs`, and is skipped without them.

use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use korpis_cli::cmd::volume::{VolumeAction, volume};
use std::path::Path;
use std::time::Duration;

/// Waits until `sql` counts `n`.
/// Plans operating on a volume that are still open, which a new one on the volume waits for.
pub(crate) const OPEN_VOLUME_PLANS: &str = "SELECT count(*) FROM plan WHERE to_volume IS NOT NULL
      AND status IN ('proposed', 'approved', 'queued', 'applying')";

pub(crate) async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..300 {
        if cp.scalar::<i64>(sql).await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{sql} never reached {n}");
}

/// The lines of `file`, once it has `n` of them.
pub(crate) async fn lines(file: &Path, n: usize) -> Vec<String> {
    for _ in 0..300 {
        let read = std::fs::read_to_string(file).unwrap_or_default();
        let lines: Vec<String> = read.lines().map(str::to_owned).collect();
        if lines.len() >= n {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} never had {n} lines", file.display());
}

/// The plan identifier a CLI answer names.
pub(crate) fn plan_in(human: &str) -> String {
    human.split_whitespace().find(|t| t.starts_with("pln_")).unwrap().to_owned()
}

#[tokio::test]
async fn a_griefed_world_is_rolled_back_to_its_snapshot() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let Some(node) = Node::join_on_btrfs(&cp, "fra-4").await else { return };
    let agent = node.agent().await;
    let w = cp.declare(&project, "world", &push_sleeper().await).await;
    node.guard(&w);
    let vol = node.volumes.join(format!("{w}.data"));
    assert_eq!(lines(&vol.join("starts"), 1).await.len(), 1);

    let actions = [
        "volume.read",
        "volume.snapshot.create",
        "volume.snapshot.rollback",
        "volume.snapshot.delete",
    ];
    let operator = cp.cli(Some(&cp.token_for(&actions).await));
    let data = cp.volume_named(&project, "data").await;
    let asked = VolumeAction::Snapshot { volume: data, name: "before-grief".into() };
    let taken = volume(&operator, &asked).await.unwrap().human;
    let snapshot = taken.split_whitespace().nth(1).unwrap().to_owned();
    counted(&cp, "SELECT count(*) FROM snapshot WHERE deleted_at IS NULL", 1).await;
    // The snapshot is recorded as the node reports it, and its plan is applied after; a rollback
    // asked before then finds the volume busy with it.
    let applied = "SELECT count(*) FROM plan WHERE kind = 'snapshot_volume' AND status = 'applied'";
    counted(&cp, applied, 1).await;
    let kept = node.volumes.join(format!(".snapshots/{w}.data"));
    assert_eq!(std::fs::read_dir(&kept).unwrap().count(), 1, "on the node, out of the volume");

    std::fs::write(vol.join("griefed"), "everything burned").unwrap();
    let back = VolumeAction::Rollback { snapshot: snapshot.clone() };
    cp.approve(&plan_in(&volume(&operator, &back).await.unwrap().human)).await;
    let sql = "SELECT count(*) FROM plan WHERE kind = 'rollback_volume' AND status = 'applied'";
    counted(&cp, sql, 1).await;
    assert!(!vol.join("griefed").exists(), "what was written since is gone");
    let starts = lines(&vol.join("starts"), 2).await;
    assert_eq!(starts.len(), 2, "the snapshot's one start, and the start after the rollback");

    let gone = VolumeAction::DeleteSnapshot { snapshot };
    cp.approve(&plan_in(&volume(&operator, &gone).await.unwrap().human)).await;
    counted(&cp, "SELECT count(*) FROM snapshot WHERE deleted_at IS NULL", 0).await;
    assert_eq!(std::fs::read_dir(&kept).unwrap().count(), 0, "and gone from the node");
    agent.stop().await;
}
