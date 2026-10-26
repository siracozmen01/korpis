// SPDX-License-Identifier: AGPL-3.0-or-later

//! What reaches a workload from outside its node (§4 of docs/07-networking.md, Finding 180 of
//! docs/23-walkthroughs.md). The node is a network namespace of the test's own, the helper making
//! the workload's network from a thread inside it, and outside it is another, joined to it by a
//! veth and routing the workload's pool through it, as a network that routes a node's prefix to
//! it does; so nothing is made in this machine's own namespace. From outside, a port whose
//! exposure is reached at the workload's own address is reached, one reached by other workloads
//! alone is not, and is counted under `exposure`, and what the node translated from a port of its
//! own is let in. Needs root and nftables; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_helper::network::Network;
use korpis_proto::korpis::helper::v1 as helper;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A network namespace of the test's own, removed when dropped.
struct Netns(String);

impl Netns {
    fn new(name: String) -> Self {
        assert!(ip(&["netns", "add", &name]), "netns {name}");
        assert!(ip(&["-n", &name, "link", "set", "lo", "up"]));
        Self(name)
    }

    /// `args` run inside it; whether they succeeded.
    fn run(&self, args: &[&str]) -> bool {
        let mut all = vec!["netns", "exec", &self.0];
        all.extend_from_slice(args);
        ip(&all)
    }

    /// `f` run on a thread inside it.
    fn within<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let path = Path::new("/run/netns").join(&self.0);
        std::thread::spawn(move || {
            let ns = std::fs::File::open(path).unwrap();
            rustix::thread::move_into_link_name_space(
                std::os::fd::AsFd::as_fd(&ns),
                Some(rustix::thread::LinkNameSpaceType::Network),
            )
            .unwrap();
            f()
        })
        .join()
        .unwrap()
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

/// The node and the network outside it: the node at `198.18.2.1`, outside at `198.18.2.2`,
/// routing `pool` through the node, which forwards.
fn machines(pool: &str) -> (Netns, Netns) {
    let id = std::process::id();
    let (node, outside) =
        (Netns::new(format!("korpis-in-{id}-n")), Netns::new(format!("korpis-in-{id}-o")));
    let (vn, vo) = (format!("kin{}n", id % 100_000), format!("kin{}o", id % 100_000));
    assert!(ip(&[
        "link", "add", &vn, "netns", &node.0, "type", "veth", "peer", "name", &vo, "netns",
        &outside.0
    ]));
    for (ns, link, address) in [(&node, &vn, "198.18.2.1/30"), (&outside, &vo, "198.18.2.2/30")] {
        assert!(ns.run(&["ip", "addr", "add", address, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
    // The test's own namespace forwards, as a node does; this machine's setting is left be.
    assert!(node.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    assert!(outside.run(&["ip", "route", "add", pool, "via", "198.18.2.1"]));
    (node, outside)
}

/// Listens over TCP on `port` inside the namespace at `path` for ten seconds.
fn listen(path: &Path, port: u16) -> std::process::Child {
    let script = format!(
        "import socket,time;s=socket.socket();s.bind(('0.0.0.0',{port}));s.listen();time.sleep(10)"
    );
    Command::new("nsenter")
        .arg(format!("--net={}", path.display()))
        .args(["python3", "-c", &script])
        .spawn()
        .unwrap()
}

/// Whether a TCP connection from inside `ns` to `to` is made within a second.
fn reached(ns: &Netns, to: &str) -> bool {
    let (host, port) = to.split_once(':').unwrap();
    let script = format!("import socket\nsocket.create_connection(('{host}',{port}),1)");
    ns.run(&["python3", "-c", &script])
}

/// What the rows of `ns`'s host table commented `comment` have counted, in packets.
fn counted(ns: &Netns, comment: &str) -> u64 {
    let args = ["netns", "exec", &ns.0, "nft", "-j", "list", "table", "inet", "korpis"];
    let out = Command::new("ip").args(args).output().unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let items =
        listing.get("nftables").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    items
        .iter()
        .filter(|i| i.pointer("/rule/comment").and_then(|c| c.as_str()) == Some(comment))
        .flat_map(|i| {
            i.pointer("/rule/expr").and_then(|e| e.as_array()).cloned().unwrap_or_default()
        })
        .filter_map(|e| e.pointer("/counter/packets").and_then(serde_json::Value::as_u64))
        .sum()
}

fn request(workload: &str) -> helper::EnsureNetworkRequest {
    let tcp = helper::Transport::TRANSPORT_TCP;
    let port = |port: u32, outside: bool| helper::Port {
        transport: tcp.into(),
        port,
        outside,
        ..Default::default()
    };
    helper::EnsureNetworkRequest {
        workload: workload.into(),
        ipv4: "10.243.0.2".into(),
        ipv4_pool: "10.243.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        // A `node_port` endpoint's, and an `overlay_only` one's, which a port of the node is
        // forwarded to here so that what the node translates is seen to be let in.
        inbound: vec![port(7001, true), port(7002, false)],
        forwards: vec![helper::Forward {
            transport: tcp.into(),
            node_port: 31_002,
            container_port: 7002,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn from_outside_the_node_only_exposed_or_translated_ports_are_reached() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("networks need root: not run");
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root: PathBuf = tmp.path().join("netns");
    let (node, outside) = machines("10.243.0.0/16");
    let workload = format!("net-in-{}", std::process::id());
    let (r, at) = (request(&workload), root.clone());
    let made = node.within(move || Network::new(at).ensure(&r));
    let made = match made {
        Ok(made) => made,
        Err(e) => {
            eprintln!("this machine offers no network ({e}): not run");
            return;
        }
    };
    let path = Path::new(&made.namespaces).join("net");
    let (mut direct, mut workloads) = (listen(&path, 7001), listen(&path, 7002));
    std::thread::sleep(Duration::from_millis(500));
    let before = counted(&node, "exposure");
    let exposed = reached(&outside, "10.243.0.2:7001");
    let unexposed = reached(&outside, "10.243.0.2:7002");
    let translated = reached(&outside, "198.18.2.1:31002");
    for child in [&mut direct, &mut workloads] {
        let _ = child.kill();
        let _ = child.wait();
    }
    let dropped = counted(&node, "exposure") > before;
    let released = node.within(move || Network::new(root).release(&workload));
    assert!(exposed, "a port reached at the workload's own address is reached from outside");
    assert!(!unexposed, "a port reached by other workloads alone is not");
    assert!(dropped, "and what tried is counted");
    assert!(translated, "what the node translated from a port of its own is let in");
    assert!(released.unwrap());
}
