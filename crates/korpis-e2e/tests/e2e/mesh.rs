// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh on real nodes (§9.1 of docs/07-networking.md, trace 74 of docs/23-walkthroughs.md):
//! two nodes, each with its data plane (its helper, its workloads' networks, its mesh
//! interface) in a network namespace of the test's own, the two joined by a veth as two machines
//! by a network, so nothing is made in this machine's own namespace and no port is opened on its
//! addresses. A web application on one reaches its database on the other through the mesh, at
//! the address the database's node gave it and on the port its endpoint declares alone; and the
//! database's node drops, and counts, what the mesh carries that it did not admit, even from an
//! address of the other node's own pool. Needs root, containerd, and the kernel's WireGuard;
//! skipped without them.

use crate::dependencies::{address_of, running};
use crate::node::{Node, live};
use crate::recipes::push_server;
use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A network namespace of the test's own, standing in for a machine; removed when dropped.
pub(crate) struct Netns(String);

impl Netns {
    pub(crate) fn new(name: String) -> Self {
        assert!(ip(&["netns", "add", &name]), "netns {name}");
        assert!(ip(&["-n", &name, "link", "set", "lo", "up"]));
        Self(name)
    }

    fn path(&self) -> PathBuf {
        Path::new("/run/netns").join(&self.0)
    }

    /// `args` run inside it; whether they succeeded.
    pub(crate) fn run(&self, args: &[&str]) -> bool {
        let mut all = vec!["netns", "exec", &self.0];
        all.extend_from_slice(args);
        ip(&all)
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = ip(&["netns", "del", &self.0]);
    }
}

fn ip(args: &[&str]) -> bool {
    Command::new("ip").args(args).output().is_ok_and(|o| o.status.success())
}

/// Two machines' namespaces joined by a veth, each forwarding as a node does: A at
/// `198.18.1.1`, B at `198.18.1.2`. `None` where this kernel makes no WireGuard link.
fn machines() -> Option<(Netns, Netns)> {
    let id = std::process::id();
    let (a, b) =
        (Netns::new(format!("korpis-mesh-{id}-a")), Netns::new(format!("korpis-mesh-{id}-b")));
    if !(a.run(&["ip", "link", "add", "kmesh-probe", "type", "wireguard"])
        && a.run(&["ip", "link", "del", "kmesh-probe"]))
    {
        return None;
    }
    let (va, vb) = (format!("kmesh{}a", id % 100_000), format!("kmesh{}b", id % 100_000));
    assert!(ip(&[
        "link", "add", &va, "netns", &a.0, "type", "veth", "peer", "name", &vb, "netns", &b.0
    ]));
    for (ns, link, address) in [(&a, &va, "198.18.1.1/30"), (&b, &vb, "198.18.1.2/30")] {
        assert!(ns.run(&["ip", "addr", "add", address, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
        // The test's own namespace forwards, as a node does; this machine's setting is left be.
        assert!(ns.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    }
    Some((a, b))
}

/// A node named `name` whose data plane is in `ns`, at `address` there.
pub(crate) async fn node_in(cp: &ControlPlane, name: &str, ns: &Netns, address: Ipv4Addr) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let volumes = dir.path().join("volumes");
    let inside = Some((ns.path(), address));
    let inside = inside.as_ref().map(|(p, a)| (p.as_path(), *a));
    Node::join_with(cp, name, dir, (volumes, None), Vec::new(), inside).await
}

/// What is answered asking for `/` over TCP at `to`, from `source` where one is named, from
/// inside the network namespace at `net`; empty if nothing answers within two seconds.
pub(crate) async fn fetched(net: &str, to: &str, source: Option<&str>) -> String {
    let (host, port) = to.split_once(':').unwrap();
    let bind = source.map(|s| format!(" s.bind(('{s}', 0))\n")).unwrap_or_default();
    let script = format!(
        "import socket\ntry:\n s=socket.socket()\n{bind} s.settimeout(2)\n \
         s.connect(('{host}',{port}))\n s.sendall(b'GET / HTTP/1.0\\r\\n\\r\\n')\n \
         print(s.recv(4096).decode(errors='replace'))\nexcept Exception: pass"
    );
    let net = net.to_owned();
    let run = move || Command::new("nsenter").arg(net).args(["python3", "-c", &script]).output();
    let out = tokio::task::spawn_blocking(run).await.unwrap().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What is answered at `to` from inside `net`, asking each second for up to a minute until the
/// server's page is.
pub(crate) async fn served(net: &str, to: &str) -> String {
    let mut answered = String::new();
    for _ in 0..60 {
        answered = fetched(net, to, None).await;
        if answered.contains("served") {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    answered
}

/// The `--net` argument entering `w`'s own network namespace on `node`.
pub(crate) fn net_of(node: &Node, w: &str) -> String {
    format!("--net={}", node.netns.join(w).join("net").display())
}

/// What `chain` of the host table in `ns` has counted under `comment`, in packets.
fn counted(ns: &Netns, chain: &str, comment: &str) -> u64 {
    let args = ["netns", "exec", &ns.0, "nft", "-j", "list", "table", "inet", "korpis"];
    let out = Command::new("ip").args(args).output().unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = listing["nftables"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .filter(|i| i.pointer("/rule/chain").and_then(|c| c.as_str()) == Some(chain))
        .filter(|i| i.pointer("/rule/comment").and_then(|c| c.as_str()) == Some(comment))
        .flat_map(|i| {
            i.pointer("/rule/expr").and_then(|e| e.as_array()).cloned().unwrap_or_default()
        })
        .filter_map(|e| e.pointer("/counter/packets").and_then(serde_json::Value::as_u64))
        .sum()
}

/// Waits until the API says `node` is in the mesh and reaches every member it is paired with.
async fn until_available(cp: &ControlPlane, node: &str) {
    for _ in 0..120 {
        let request = pb::GetNodeRequest { id: node.to_owned(), ..Default::default() };
        let got = cp.nodes().get_node(request).await.unwrap().into_owned();
        if got.node.as_option().is_some_and(|n| n.overlay == pb::Overlay::OVERLAY_AVAILABLE) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("node {node} never reached its peers through the mesh");
}

/// A body declaring the server recipe's endpoint with `exposure`.
fn exposed(exposure: pb::Exposure) -> pb::IntentBody {
    let http =
        pb::EndpointSpec { name: "http".into(), exposure: exposure.into(), ..Default::default() };
    pb::IntentBody { endpoints: vec![http], ..Default::default() }
}

/// Cordons `node`, so nothing more is placed there.
async fn cordon(cp: &ControlPlane, node: &str) {
    let request = pb::CordonNodeRequest {
        mutation: pb::Mutation { idempotency_key: format!("cordon-{node}"), ..Default::default() }
            .into(),
        node: node.to_owned(),
        ..Default::default()
    };
    cp.nodes().cordon_node(request).await.unwrap();
}

/// That what `there`'s node sends through the mesh to `to` from an address of its own pool that
/// is no admitted dependent, as a compromised node would, is dropped by `here`'s, and counted.
async fn unadmitted_is_dropped((there, there_ns): (&Node, &Netns), here_ns: &Netns, to: &str) {
    let pool: ipnet::Ipv4Net = there.state.load().unwrap().keys.workload_ipv4.parse().unwrap();
    let spoofed = pool.hosts().last().unwrap().to_string();
    assert!(there_ns.run(&["ip", "addr", "add", &format!("{spoofed}/32"), "dev", "lo"]));
    let before = counted(here_ns, "overlay_forward", "overlay");
    let net = format!("--net={}", there_ns.path().display());
    // A loaded machine may hold the first attempt behind the mesh's handshake, past its timeout:
    // every attempt is refused, and one of them is counted where it is dropped.
    for _ in 0..10 {
        assert!(fetched(&net, to, Some(&spoofed)).await.is_empty(), "not admitted");
        if counted(here_ns, "overlay_forward", "overlay") > before {
            return;
        }
    }
    panic!("dropped, and counted");
}

#[tokio::test]
async fn a_web_application_reaches_its_database_on_another_node_through_the_mesh_alone() {
    if !live() {
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
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let a = node_in(&cp, "mesh-a", &ma, Ipv4Addr::new(198, 18, 1, 1)).await;
    let b = node_in(&cp, "mesh-b", &mb, Ipv4Addr::new(198, 18, 1, 2)).await;
    let (agent_a, agent_b) = (a.agent().await, b.agent().await);
    let named = |n: &Node| n.state.load().unwrap().node;
    until_available(&cp, &named(&a)).await;
    until_available(&cp, &named(&b)).await;

    let repo = push_server().await;
    // A node in a namespace of the test's own has no public address (§2.1 of
    // docs/05-scheduling.md): the database is reached through the mesh alone, as its exposure
    // says, and nothing reaches the web application in this test.
    let db =
        cp.declare_body(&project, "db", &repo, exposed(pb::Exposure::EXPOSURE_OVERLAY_ONLY)).await;
    a.guard(&db);
    b.guard(&db);
    running(&db).await;
    let (here, there, here_ns, there_ns) =
        if address_of(&a, &db).is_some() { (&a, &b, &ma, &mb) } else { (&b, &a, &mb, &ma) };
    cordon(&cp, &named(here)).await;
    let dependency = pb::Dependency {
        name: "db".into(),
        target: Some(Target::Workload(db.clone())),
        required: true,
        ..Default::default()
    };
    let body =
        pb::IntentBody { dependencies: vec![dependency], ..exposed(pb::Exposure::EXPOSURE_NONE) };
    let web = cp.declare_body(&project, "web", &repo, body).await;
    a.guard(&web);
    b.guard(&web);
    running(&web).await;
    assert!(address_of(there, &web).is_some(), "the web application is on the other node");

    let db_ip = address_of(here, &db).expect("the database's address");
    let (to, beside) = (format!("{db_ip}:8080"), format!("{db_ip}:8081"));
    let answered = served(&net_of(there, &web), &to).await;
    assert!(answered.contains("served"), "through the mesh to its database: {answered:?}");
    assert!(counted(here_ns, "overlay_forward", "dependency") > 0, "admitted, and counted");
    assert!(
        fetched(&net_of(there, &web), &beside, None).await.is_empty(),
        "a port its database's endpoint does not declare is not reached"
    );

    unadmitted_is_dropped((there, there_ns), here_ns, &to).await;

    for w in [&web, &db] {
        cp.retire(w).await;
    }
    agent_a.stop().await;
    agent_b.stop().await;
}
