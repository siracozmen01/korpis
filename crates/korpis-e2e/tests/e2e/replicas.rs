// SPDX-License-Identifier: AGPL-3.0-or-later

//! A replicated volume on real nodes (§2.2 of docs/06-storage.md, trace 80 of
//! docs/23-walkthroughs.md): two nodes, each with its volumes on a btrfs of its own and its data
//! plane in a network namespace of the test's own, joined by a veth as two machines by a network.
//! A workload whose volume asks for one replica runs on the first, which writes into the volume;
//! once the second joins, the control plane chooses it, the first sends it the volume's snapshot
//! whole through the mesh, the second's table admitting to the transfer port the first's mesh
//! address alone, and the second holds what the volume held, read-only, and says so. Needs
//! root, containerd, the kernel's WireGuard, and btrfs-progs; skipped without them.

use crate::helper::btrfs_in;
use crate::mesh::{Netns, machines, until_available};
use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A node named `name` with its volumes on a btrfs of its own and its data plane in `ns`, at
/// `address` there; `None` where this machine makes no btrfs.
async fn node_on_btrfs_in(
    cp: &ControlPlane,
    name: &str,
    ns: &Netns,
    address: Ipv4Addr,
) -> Option<Node> {
    let dir = tempfile::tempdir().unwrap();
    let at = btrfs_in(dir.path())?;
    let volumes = at.join("volumes");
    let inside = Some((ns.path(), address));
    let inside = inside.as_ref().map(|(p, a)| (p.as_path(), *a));
    Some(Node::join_with(cp, name, dir, (volumes, Some(at), None), Vec::new(), inside).await)
}

/// The volume `id`, as the API shows it.
async fn shown(cp: &ControlPlane, id: &str) -> pb::Volume {
    let client = pb::VolumeServiceClient::new(cp.http(), cp.config());
    let asked = pb::GetVolumeRequest { id: id.to_owned(), ..Default::default() };
    let got = client.get_volume(asked).await.unwrap().into_owned().volume;
    got.into_option().unwrap()
}

/// The only entry of `dir` whose name `keep` accepts, once there is one.
async fn only_entry(dir: &Path, keep: impl Fn(&str) -> bool) -> PathBuf {
    for _ in 0..300 {
        let found: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| keep(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        if let [one] = found.as_slice() {
            return one.clone();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} never held one entry", dir.display());
}

/// The mesh addresses the table in `ns` admits to the transfer port.
fn admitted(ns: &Netns) -> Vec<String> {
    let args = ["netns", "exec", ns.name(), "nft", "-j", "list", "set", "inet", "korpis"];
    let out = std::process::Command::new("ip").args(args).arg("overlay_transfer").output().unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let set = listing["nftables"].as_array().and_then(|i| i.iter().find_map(|i| i.get("set")));
    let elements = set.and_then(|s| s["elem"].as_array()).cloned().unwrap_or_default();
    elements.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect()
}

#[tokio::test]
async fn a_volume_is_sent_whole_to_its_replica_through_the_mesh() {
    if !live() || std::fs::metadata("/usr/bin/btrfs").is_err() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let Some((ma, mb)) = machines() else {
        eprintln!("this kernel makes no WireGuard link: not run");
        return;
    };
    let cp = ControlPlane::start_beating(Duration::from_secs(1)).await;
    let project = cp.project().await;
    let Some(a) = node_on_btrfs_in(&cp, "rep-a", &ma, Ipv4Addr::new(198, 18, 1, 1)).await else {
        return;
    };
    let agent_a = a.agent().await;
    let repo = push_sleeper().await;
    let spec = pb::VolumeSpec { name: "data".into(), replicas: 1, ..Default::default() };
    let w = cp.declare_with(&project, "db", &repo, vec![spec]).await;
    a.guard(&w);
    let data = cp.volume_named(&project, "data").await;
    // Written before any replica can be chosen, so the first snapshot holds it.
    let volume =
        only_entry(&a.volumes, |n| n.split_once('.').is_some_and(|(_, v)| v == "data")).await;
    std::fs::write(volume.join("marker"), "written on a").unwrap();

    let b = node_on_btrfs_in(&cp, "rep-b", &mb, Ipv4Addr::new(198, 18, 1, 2)).await.unwrap();
    let agent_b = b.agent().await;
    let named = |n: &Node| n.state.load().unwrap().node;
    until_available(&cp, &named(&a)).await;
    until_available(&cp, &named(&b)).await;
    let mut held = shown(&cp, &data).await;
    for _ in 0..180 {
        if held.replicas.first().is_some_and(|r| r.received_at.is_set()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        held = shown(&cp, &data).await;
    }
    let replica = held.replicas.first().expect("a replica chosen");
    assert!(replica.received_at.is_set(), "never received: {held:?}");
    assert_eq!(replica.node, named(&b), "on the other node");
    let kept = b.volumes.join(".replicas").join(&data);
    let snapshot = only_entry(&kept, |_| true).await;
    let marker = std::fs::read_to_string(snapshot.join("marker")).unwrap();
    assert_eq!(marker, "written on a", "what the volume held");
    assert!(std::fs::write(snapshot.join("other"), "x").is_err(), "read-only");
    // The table is written whole at each pass, so what it admits is read, not what it counted.
    let pool: ipnet::Ipv4Net = a.state.load().unwrap().keys.workload_ipv4.parse().unwrap();
    let sender = korpis_core::edge::mesh_address(&[ipnet::IpNet::V4(pool)]).unwrap();
    assert_eq!(admitted(&mb), [sender.to_string()], "the sending node's mesh address alone");
    agent_b.stop().await;
    agent_a.stop().await;
}
