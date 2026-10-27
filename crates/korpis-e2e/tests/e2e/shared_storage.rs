// SPDX-License-Identifier: AGPL-3.0-or-later

//! A shared storage class on real nodes (§2.1 of docs/06-storage.md, trace 80 of
//! docs/23-walkthroughs.md). Two nodes offer a class of one store: a directory both mount, as two
//! machines mount one network filesystem, which the helper takes as shared only in tests. The
//! first node's helper names the store and the second reads it. A workload whose volume is in
//! the class runs on the first node and writes to it; once it is retired and the first node
//! cordoned, a workload attaching that volume runs on the second, which finds what the first
//! wrote, in the store and not on either node's disk. And when the node a workload holding such
//! a volume runs on loses its power, the workload runs on the other once its lease has ended,
//! placed there by failover, its data where it was (§2.4 of docs/05-scheduling.md). Needs root
//! and containerd.

use crate::mesh::cordon;
use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::path::Path;
use std::time::Duration;

/// The lines of the file at `path`, within three minutes of there being `n`, which `w` writes;
/// what the control plane says of `w` otherwise.
pub(crate) async fn lines_reach(
    (cp, w): (&ControlPlane, &str),
    path: &Path,
    n: usize,
) -> Vec<String> {
    let read =
        || std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_owned).collect();
    for _ in 0..900 {
        let lines: Vec<String> = read();
        if lines.len() >= n {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let status = cp.status(w).await;
    panic!("{} never held {n} lines: {:?}; {w}: {status:?}", path.display(), read());
}

/// The volume `id`, as the API shows it.
async fn shown(cp: &ControlPlane, id: &str) -> pb::Volume {
    let client = pb::VolumeServiceClient::new(cp.http(), cp.config());
    let asked = pb::GetVolumeRequest { id: id.to_owned(), ..Default::default() };
    let got = client.get_volume(asked).await.unwrap().into_owned().volume;
    got.into_option().unwrap()
}

/// The one volume's directory in the shared store at `root`, beside the store's marker.
fn in_store(root: &Path) -> std::path::PathBuf {
    let dirs: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    dirs.into_iter().next().unwrap()
}

/// Waits until the API shows `node` offering the class `class` with its store, which its first
/// session declares.
pub(crate) async fn until_offering(cp: &ControlPlane, node: &str, class: &str) {
    for _ in 0..120 {
        let request = pb::GetNodeRequest { id: node.to_owned(), ..Default::default() };
        let got = cp.nodes().get_node(request).await.unwrap().into_owned().node;
        let classes = got.into_option().map(|n| n.storage_classes).unwrap_or_default();
        if classes.iter().any(|c| c.name == class && !c.store.is_empty()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("node {node} never offered the class {class}");
}

/// Lifts the cordon on `node`.
pub(crate) async fn uncordon(cp: &ControlPlane, node: &str) {
    let request = pb::UncordonNodeRequest {
        mutation: pb::Mutation {
            idempotency_key: format!("uncordon-{node}"),
            ..Default::default()
        }
        .into(),
        node: node.to_owned(),
        ..Default::default()
    };
    cp.nodes().uncordon_node(request).await.unwrap();
}

#[tokio::test]
async fn a_volume_in_a_shared_store_is_found_from_the_other_node() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let shared = tempfile::tempdir().unwrap();
    let (a, b) = (
        Node::join_sharing(&cp, "store-a", shared.path()).await,
        Node::join_sharing(&cp, "store-b", shared.path()).await,
    );
    let (agent_a, agent_b) = (a.agent().await, b.agent().await);
    let named = |n: &Node| n.state.load().unwrap().node;
    let (na, nb) = (named(&a), named(&b));
    for n in [&na, &nb] {
        until_offering(&cp, n, "shared").await;
    }
    cordon(&cp, &nb).await;
    let project = cp.project().await;
    let repo = push_sleeper().await;
    let spec = pb::VolumeSpec {
        name: "data".into(),
        storage_class: "shared".into(),
        ..Default::default()
    };
    let first = cp.declare_with(&project, "first", &repo, vec![spec]).await;
    a.guard(&first);
    let data = cp.volume_named(&project, "data").await;
    let volume = shown(&cp, &data).await;
    assert!(volume.node.is_none() && !volume.store.is_empty(), "the store's: {volume:?}");
    let marker = std::fs::read_to_string(shared.path().join(".korpis-store")).unwrap();
    assert_eq!(volume.store, marker.trim(), "as the first helper named it, and the second read it");
    let starts = in_store(shared.path()).join("starts");
    lines_reach((&cp, &first), &starts, 1).await;
    cp.retire(&first).await;
    for _ in 0..300 {
        if shown(&cp, &data).await.attached_to.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    cordon(&cp, &na).await;
    uncordon(&cp, &nb).await;
    let attach = pb::VolumeSpec { name: "data".into(), existing: Some(data), ..Default::default() };
    let second = cp.declare_with(&project, "second", &repo, vec![attach]).await;
    b.guard(&second);
    let lines = lines_reach((&cp, &second), &starts, 2).await;
    assert_eq!(lines.len(), 2, "its start is added to the first's, from the other node");
    // Each node's own disk holds its workload's console stream, and nothing of `data`.
    let held = |n: &Node| {
        let entries = std::fs::read_dir(&n.volumes).unwrap().filter_map(Result::ok);
        let entries = entries.map(|e| e.file_name().to_string_lossy().into_owned());
        entries
            .filter(|n| !n.starts_with('.') && !n.ends_with(".stream_console"))
            .collect::<Vec<_>>()
    };
    let (on_a, on_b) = (held(&a), held(&b));
    assert!(on_a.is_empty() && on_b.is_empty(), "on neither node's disk: {on_a:?} {on_b:?}");
    cp.retire(&second).await;
    agent_a.stop().await;
    agent_b.stop().await;
}

#[tokio::test]
async fn a_workload_whose_node_loses_power_runs_on_the_other_with_its_data() {
    if !live() {
        return;
    }
    // A short heartbeat, so the node is unreachable seconds after its power is cut rather than a
    // minute and a half; its lease's own term still governs when the workload moves.
    let cp = ControlPlane::start_beating(Duration::from_secs(3)).await;
    let shared = tempfile::tempdir().unwrap();
    let (a, b) = (
        Node::join_sharing(&cp, "lost-a", shared.path()).await,
        Node::join_sharing(&cp, "kept-b", shared.path()).await,
    );
    let (agent_a, agent_b) = (a.agent().await, b.agent().await);
    let named = |n: &Node| n.state.load().unwrap().node;
    let (na, nb) = (named(&a), named(&b));
    for n in [&na, &nb] {
        until_offering(&cp, n, "shared").await;
    }
    cordon(&cp, &nb).await;
    let project = cp.project().await;
    let repo = push_sleeper().await;
    let spec = pb::VolumeSpec {
        name: "data".into(),
        storage_class: "shared".into(),
        ..Default::default()
    };
    let db = cp.declare_with(&project, "db", &repo, vec![spec]).await;
    a.guard(&db);
    b.guard(&db);
    let starts = in_store_once(shared.path()).await.join("starts");
    lines_reach((&cp, &db), &starts, 1).await;
    uncordon(&cp, &nb).await;

    // The power is cut: its agent ends without a word, and what it ran ends with it.
    drop(agent_a);
    crate::containers::remove_workload(&db);
    let lines = lines_reach((&cp, &db), &starts, 2).await;
    assert_eq!(lines.len(), 2, "started again, from the other node, on what the first wrote");
    let placed = cp.status(&db).await.placement.into_option().unwrap();
    assert_eq!(placed.node, nb, "on the node that remains");
    assert_eq!(placed.bound_by.as_known(), Some(pb::BoundBy::BOUND_BY_FAILOVER));
    cp.retire(&db).await;
    agent_b.stop().await;
}

/// The one volume's directory in the shared store at `root`, once the workload's node made it.
async fn in_store_once(root: &Path) -> std::path::PathBuf {
    for _ in 0..600 {
        let made = std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'));
        if made {
            return in_store(root);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("no volume was made in {}", root.display());
}
