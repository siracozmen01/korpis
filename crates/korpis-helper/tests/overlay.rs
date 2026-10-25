// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh's interface, made by the helper (§9.1 of docs/07-networking.md), between two
//! network namespaces of the test's own joined by a veth pair, each standing in for a node's
//! host, so no interface is made in this machine's own namespace and no port opened on its
//! addresses. Each node has a workload, a namespace of its own behind a link with an address of
//! its node's pool, which the node forwards for, as Korpis's own links are. B's workload reaches
//! A's through the mesh on the one port A admits it to and on no other, and only once admitted;
//! peers and routes no longer asked for are removed. Needs root and the kernel's WireGuard, which
//! the kernel loads when the first such link is asked for; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_helper::overlay::{INTERFACE, Overlay};
use korpis_proto::korpis::helper::v1 as helper;
use std::net::{SocketAddr, TcpListener, TcpStream};
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

    fn path(&self) -> PathBuf {
        Path::new("/run/netns").join(&self.0)
    }

    /// `args` run inside it; whether they succeeded.
    fn run(&self, args: &[&str]) -> bool {
        let mut all = vec!["netns", "exec", &self.0];
        all.extend_from_slice(args);
        ip(&all)
    }

    /// `f` run on a thread inside it.
    fn within<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let path = self.path();
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

    /// The overlay kept to `r` from a thread inside it.
    fn keep(
        &self,
        overlay: &Overlay,
        r: helper::KeepOverlayRequest,
    ) -> helper::KeepOverlayResponse {
        let overlay = overlay.clone();
        self.within(move || overlay.keep(&r).unwrap())
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

/// Joins `node` and `workload` with a veth named `link` on the node's side: the node holds
/// `gateway`, the workload `address` with its default route through it, and the node forwards.
fn workload(node: &Netns, workload: &Netns, link: &str, (gateway, address): (&str, &str)) {
    let inner = format!("{link}w");
    assert!(ip(&[
        "link",
        "add",
        link,
        "netns",
        &node.0,
        "type",
        "veth",
        "peer",
        "name",
        &inner,
        "netns",
        &workload.0
    ]));
    assert!(node.run(&["ip", "addr", "add", &format!("{gateway}/30"), "dev", link]));
    assert!(node.run(&["ip", "link", "set", link, "up"]));
    assert!(workload.run(&["ip", "addr", "add", &format!("{address}/30"), "dev", &inner]));
    assert!(workload.run(&["ip", "link", "set", &inner, "up"]));
    assert!(workload.run(&["ip", "route", "add", "default", "via", gateway]));
    // The test's own namespace forwards, as a node does; the host's setting is left alone.
    assert!(node.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
}

fn peer(key: &[u8], endpoint: &str, pool: &str) -> helper::OverlayPeer {
    helper::OverlayPeer {
        public_key: key.to_vec(),
        endpoint: endpoint.into(),
        allowed_ips: vec![pool.into()],
        ..Default::default()
    }
}

fn request(
    peers: Vec<helper::OverlayPeer>,
    admitted: Vec<helper::OverlayAdmitted>,
) -> helper::KeepOverlayRequest {
    helper::KeepOverlayRequest { port: 51820, peers, admitted, ..Default::default() }
}

/// B's workload admitted to A's on `port`.
fn admit(port: u32) -> helper::OverlayAdmitted {
    helper::OverlayAdmitted {
        source: "10.48.4.2".into(),
        destination: "10.48.0.2".into(),
        port,
        transport: helper::Transport::TRANSPORT_TCP.into(),
        ..Default::default()
    }
}

/// Whether B's workload connects to A's on `port`.
fn connects(wb: &Netns, port: u16) -> bool {
    let to = SocketAddr::from(([10, 48, 0, 2], port));
    wb.within(move || TcpStream::connect_timeout(&to, Duration::from_secs(2)).is_ok())
}

#[test]
fn a_dependent_reaches_its_target_s_admitted_port_through_the_mesh_and_nothing_else() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("the mesh needs root: not run");
        return;
    }
    let id = std::process::id();
    let named = |n: &str| Netns::new(format!("korpis-ovl-{id}-{n}"));
    let (a, b, wa, wb) = (named("a"), named("b"), named("wa"), named("wb"));
    let (va, vb) = (format!("kovl{}a", id % 100_000), format!("kovl{}b", id % 100_000));
    assert!(ip(&[
        "link", "add", &va, "netns", &a.0, "type", "veth", "peer", "name", &vb, "netns", &b.0
    ]));
    for (ns, link, addr) in [(&a, &va, "198.18.0.1/30"), (&b, &vb, "198.18.0.2/30")] {
        assert!(ns.run(&["ip", "addr", "add", addr, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
    if !(a.run(&["ip", "link", "add", "kovl-probe", "type", "wireguard"])
        && a.run(&["ip", "link", "del", "kovl-probe"]))
    {
        eprintln!("this kernel makes no WireGuard link: not run");
        return;
    }
    workload(&a, &wa, &format!("kw{}a", id % 100_000), ("10.48.0.1", "10.48.0.2"));
    workload(&b, &wb, &format!("kw{}b", id % 100_000), ("10.48.4.1", "10.48.4.2"));
    let listening: Vec<TcpListener> = [5432, 5433]
        .map(|p| wa.within(move || TcpListener::bind(("10.48.0.2", p)).unwrap()))
        .into();

    let dir = tempfile::tempdir().unwrap();
    let (oa, ob) = (Overlay::new(dir.path().join("a.key")), Overlay::new(dir.path().join("b.key")));
    let ka = a.keep(&oa, request(Vec::new(), Vec::new()));
    let kb = b.keep(&ob, request(Vec::new(), Vec::new()));
    assert_eq!(ka.public_key.len(), 32);
    let again = a.keep(&oa, request(Vec::new(), Vec::new()));
    assert_eq!(again.public_key, ka.public_key, "the key is kept");

    // B advertises no address: A reaches it once B has reached A, as a node behind NAT is.
    let to_b = || vec![peer(&kb.public_key, "", "10.48.4.0/22")];
    a.keep(&oa, request(to_b(), Vec::new()));
    b.keep(&ob, request(vec![peer(&ka.public_key, "198.18.0.1:51820", "10.48.0.0/22")], vec![]));
    assert!(!connects(&wb, 5432), "nothing admitted, nothing reaches A's workload");
    a.keep(&oa, request(to_b(), vec![admit(5432)]));
    assert!(connects(&wb, 5432), "B's workload reaches the port it is admitted to");
    assert!(!connects(&wb, 5433), "and no other of the same workload");
    let seen = a.keep(&oa, request(to_b(), vec![admit(5432)]));
    assert!(seen.handshakes[0].latest_unix_seconds > 0, "a handshake with B, as A reads it");
    drop(listening);

    // A peer no longer asked for is removed, and so is its route.
    a.keep(&oa, request(Vec::new(), Vec::new()));
    let routes =
        Command::new("ip").args(["-n", &a.0, "route", "show", "dev", INTERFACE]).output().unwrap();
    assert!(String::from_utf8_lossy(&routes.stdout).trim().is_empty(), "no route left");
}
