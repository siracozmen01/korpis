// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's dedicated address (§3.7 of docs/07-networking.md, trace 77 of
//! docs/23-walkthroughs.md), as the helper holds it on a node whose network puts the pool's
//! addresses on the node's link. The node is a network namespace of the test's own, the helper
//! making the workload's network from a thread inside it, and the network is another, joined to
//! it by a veth on one subnet; so nothing is made in this machine's own namespace. The address is
//! added to the node's interface and translated to the workload, its declared port reached there
//! and no other; moved to an address the operator had put on the interface already, the first is
//! taken off and the operator's left when the workload goes. Needs root and nftables; skipped
//! without them.

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

/// The node at `198.18.4.1/24` and the network at `198.18.4.2/24`, on one link; and the node's
/// side of it, by name.
fn machines() -> (Netns, Netns, String) {
    let id = std::process::id();
    let (node, outside) =
        (Netns::new(format!("korpis-ded-{id}-n")), Netns::new(format!("korpis-ded-{id}-o")));
    let (vn, vo) = (format!("kdd{}n", id % 100_000), format!("kdd{}o", id % 100_000));
    assert!(ip(&[
        "link", "add", &vn, "netns", &node.0, "type", "veth", "peer", "name", &vo, "netns",
        &outside.0
    ]));
    for (ns, link, address) in [(&node, &vn, "198.18.4.1/24"), (&outside, &vo, "198.18.4.2/24")] {
        assert!(ns.run(&["ip", "addr", "add", address, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
    // The test's own namespace forwards, as a node does; this machine's setting is left be.
    assert!(node.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    (node, outside, vn)
}

/// Whether a TCP connection from inside `ns` to `to` is made within a second.
fn reached(ns: &Netns, to: &str) -> bool {
    let (host, port) = to.split_once(':').unwrap();
    let script = format!("import socket\nsocket.create_connection(('{host}',{port}),1)");
    ns.run(&["python3", "-c", &script])
}

/// The IPv4 addresses on `link` inside `ns`, as `ip` lists them.
fn on(ns: &Netns, link: &str) -> String {
    let args = ["-n", &ns.0, "-4", "-o", "addr", "show", "dev", link];
    let out = Command::new("ip").args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn request(workload: &str, dedicated: &str, interface: &str) -> helper::EnsureNetworkRequest {
    let port = helper::Port {
        transport: helper::Transport::TRANSPORT_TCP.into(),
        port: 7001,
        ..Default::default()
    };
    helper::EnsureNetworkRequest {
        workload: workload.into(),
        ipv4: "10.243.0.2".into(),
        ipv4_pool: "10.243.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        inbound: vec![port],
        dedicated: helper::DedicatedAddress {
            address: dedicated.into(),
            interface: interface.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

#[test]
fn a_dedicated_address_on_the_link_is_answered_translated_and_given_back() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("networks need root: not run");
        return;
    }
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root: PathBuf = tmp.path().join("netns");
    let (node, outside, link) = machines();
    // An address the operator put on the interface, which is theirs whatever holds it.
    assert!(node.run(&["ip", "addr", "add", "198.18.4.20/32", "dev", &link]));
    let workload = format!("net-ded-{}", std::process::id());
    let (r, at) = (request(&workload, "198.18.4.17", &link), root.clone());
    let made = match node.within(move || Network::new(at).ensure(&r)) {
        Ok(made) => made,
        Err(e) => {
            eprintln!("this machine offers no network ({e}): not run");
            return;
        }
    };
    let mut server = listen(&Path::new(&made.namespaces).join("net"), 7001);
    std::thread::sleep(Duration::from_millis(500));
    let declared = reached(&outside, "198.18.4.17:7001");
    let undeclared = reached(&outside, "198.18.4.17:22");
    let _ = server.kill();
    let _ = server.wait();
    let added = on(&node, &link);

    let (r, at) = (request(&workload, "198.18.4.20", &link), root.clone());
    node.within(move || Network::new(at).ensure(&r)).unwrap();
    let moved = on(&node, &link);
    let w = workload.clone();
    let released = node.within(move || Network::new(root).release(&w));
    let left = on(&node, &link);

    assert!(declared, "its declared port, at its dedicated address");
    assert!(!undeclared, "and no other port there");
    assert!(added.contains("198.18.4.17/32"), "answered on the link: {added}");
    assert!(!moved.contains("198.18.4.17/"), "taken off once it holds another: {moved}");
    assert!(released.unwrap());
    assert!(left.contains("198.18.4.20/32"), "the operator's address stays: {left}");
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
