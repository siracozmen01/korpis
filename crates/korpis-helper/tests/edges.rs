// SPDX-License-Identifier: AGPL-3.0-or-later

//! An edge forwarding a stable address to a workload on another node (§3.1 of
//! docs/07-networking.md, trace 78 of docs/23-walkthroughs.md), as the helper keeps it on both.
//! A client, an edge, and a node are network namespaces of the test's own, the client and the
//! edge on one link, the edge and the node joined by the mesh over another, and the node's
//! workload a namespace behind it; so nothing is made in this machine's own namespace. The
//! client reaches the workload at the address the edge holds, and the workload sees the
//! client's own address; a port nothing is behind is refused at once; a port the node was not
//! told to admit is not reached; the address is announced on its link when taken; a UDP flow
//! follows its port when the port's target moves; GRE from the edge's mesh address arriving other than through
//! the mesh is dropped; and an address no longer held is taken off, unless it was the
//! operator's before. Needs root, the kernel's WireGuard and GRE, and nftables; skipped without
//! them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_helper::overlay::Overlay;
use korpis_proto::korpis::helper::v1 as helper;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
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

    fn keep(&self, o: &Overlay, r: helper::KeepOverlayRequest) -> helper::KeepOverlayResponse {
        let o = o.clone();
        self.within(move || o.keep(&r).unwrap())
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

/// A veth from `a` to `b`, each end named and addressed as given, both up.
fn wire(a: (&Netns, &str, &str), b: (&Netns, &str, &str)) {
    assert!(ip(&[
        "link", "add", a.1, "netns", &a.0.0, "type", "veth", "peer", "name", b.1, "netns", &b.0.0
    ]));
    for (ns, link, addr) in [a, b] {
        assert!(ns.run(&["ip", "addr", "add", addr, "dev", link]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
}

/// The edge translating `port` of the stable address to the workload's, through the node.
fn translated(port: u32) -> helper::EdgeTranslation {
    helper::EdgeTranslation {
        address: "198.18.1.10".into(),
        port,
        transport: helper::Transport::TRANSPORT_TCP.into(),
        via: "10.48.7.255".into(),
        to_address: "10.48.4.2".into(),
        to_port: port,
        ..Default::default()
    }
}

/// The edge translating UDP `port` of the stable address to the workload's `to_port`.
fn udp_translated(port: u32, to_port: u32) -> helper::EdgeTranslation {
    helper::EdgeTranslation {
        transport: helper::Transport::TRANSPORT_UDP.into(),
        to_port,
        ..translated(port)
    }
}

/// The edge's request: its peer the node, its tunnel to the node, `translations`, and `holds`.
fn edge(
    node_key: &[u8],
    translations: Vec<helper::EdgeTranslation>,
    holds: Vec<helper::EdgeHold>,
) -> helper::KeepOverlayRequest {
    helper::KeepOverlayRequest {
        port: 51820,
        peers: vec![helper::OverlayPeer {
            public_key: node_key.to_vec(),
            endpoint: "198.18.0.2:51820".into(),
            allowed_ips: vec!["10.48.4.0/22".into()],
            ..Default::default()
        }],
        mesh_address: "10.48.3.255".into(),
        edges: helper::EdgeTable {
            tunnels: vec!["10.48.7.255".into()],
            translations,
            holds,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// The node's request: its peer the edge, and the edge admitted to the workload's 25565.
fn node(edge_key: &[u8]) -> helper::KeepOverlayRequest {
    helper::KeepOverlayRequest {
        port: 51820,
        peers: vec![helper::OverlayPeer {
            public_key: edge_key.to_vec(),
            endpoint: "198.18.0.1:51820".into(),
            allowed_ips: vec!["10.48.0.0/22".into()],
            ..Default::default()
        }],
        mesh_address: "10.48.7.255".into(),
        edges: helper::EdgeTable {
            tunnels: vec!["10.48.3.255".into()],
            admits: [
                (25_565, helper::Transport::TRANSPORT_TCP),
                (25_570, helper::Transport::TRANSPORT_UDP),
                (25_571, helper::Transport::TRANSPORT_UDP),
            ]
            .map(|(port, transport)| helper::EdgeAdmit {
                source: "10.48.3.255".into(),
                destination: "10.48.4.2".into(),
                port,
                transport: transport.into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// What the client's connection to the stable address on `port` comes to, within two seconds.
fn connect(client: &Netns, port: u16) -> std::io::Result<()> {
    let to = SocketAddr::from(([198, 18, 1, 10], port));
    client.within(move || TcpStream::connect_timeout(&to, Duration::from_secs(2)).map(|_| ()))
}

fn capable(ns: &Netns) -> bool {
    let wg = ns.run(&["ip", "link", "add", "kedg-wg", "type", "wireguard"])
        && ns.run(&["ip", "link", "del", "kedg-wg"]);
    let gre = ns.run(&[
        "ip",
        "link",
        "add",
        "kedg-gre",
        "type",
        "gre",
        "local",
        "198.18.0.1",
        "remote",
        "198.18.0.2",
    ]) && ns.run(&["ip", "link", "del", "kedg-gre"]);
    wg && gre
}

/// A UDP socket bound to `at` inside `ns`, reading for two seconds at most.
fn udp_in(ns: &Netns, at: (&'static str, u16)) -> UdpSocket {
    let socket = ns.within(move || UdpSocket::bind(at).unwrap());
    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    socket
}

fn received(socket: &UdpSocket) -> Option<Vec<u8>> {
    let mut buffer = [0; 16];
    let n = socket.recv(&mut buffer).ok()?;
    buffer.get(..n).map(<[u8]>::to_vec)
}

/// A player's UDP flow to the stable address keeps going where its port goes once `retarget`
/// moves the port to the workload's other port: the edge forgets the flow's translation, so its
/// next datagram is translated afresh rather than sent where the port went before.
fn a_flow_follows_its_port(client: &Netns, w: &Netns, retarget: impl FnOnce()) {
    let (old, new) = (udp_in(w, ("10.48.4.2", 25_570)), udp_in(w, ("10.48.4.2", 25_571)));
    let player = udp_in(client, ("198.18.1.2", 40_000));
    player.send_to(b"a", "198.18.1.10:25570").unwrap();
    assert_eq!(received(&old).as_deref(), Some(&b"a"[..]), "the flow reaches the workload");
    retarget();
    player.send_to(b"b", "198.18.1.10:25570").unwrap();
    assert_eq!(received(&new).as_deref(), Some(&b"b"[..]), "the flow follows its port");
}

/// The client's entry for the stable address naming another machine, as it would after the
/// address's last holder; the client takes an announcement at once.
fn stale_neighbour(client: &Netns, link: &str) {
    assert!(client.run(&["sysctl", "-qw", &format!("net.ipv4.neigh.{link}.locktime=0")]));
    let fake = ["lladdr", "02:00:00:00:00:99", "nud", "stale"];
    let args = [&["ip", "neigh", "replace", "198.18.1.10"][..], &fake, &["dev", link]].concat();
    assert!(client.run(&args));
}

/// Whether the client's entry for the stable address names the edge's link `lan`.
fn announced(client: &Netns, e: &Netns, lan: &str) -> bool {
    let mac = |ns: &Netns, args: &[&str]| {
        let mut all = vec!["-n", &ns.0];
        all.extend_from_slice(args);
        String::from_utf8_lossy(&Command::new("ip").args(&all).output().unwrap().stdout).to_string()
    };
    let link = mac(e, &["-br", "link", "show", "dev", lan]);
    let ours = link.split_whitespace().nth(2).unwrap().to_owned();
    mac(client, &["neigh", "show", "198.18.1.10"]).contains(&ours)
}

/// A machine on the node's own network sending GRE with the edge's mesh address as its source:
/// here the edge itself, its GRE routed across the link outside the mesh. The node checks
/// sources loosely, as many hosts do, so its table alone stops it.
fn outside_the_mesh(client: &Netns, e: &Netns, n: &Netns) {
    assert!(n.run(&["sysctl", "-qw", "net.ipv4.conf.all.rp_filter=2"]));
    assert!(e.run(&["ip", "route", "add", "10.48.7.255/32", "via", "198.18.0.2", "table", "99"]));
    assert!(e.run(&["ip", "rule", "add", "to", "10.48.7.255", "ipproto", "gre", "lookup", "99"]));
    assert!(connect(client, 25_565).is_err(), "GRE from outside the mesh reaches no link");
}

/// The addresses the edge holds on `lan`: the stable address, and one the interface had already.
fn held(lan: &str) -> Vec<helper::EdgeHold> {
    ["198.18.1.10", "198.18.1.11"]
        .map(|a| helper::EdgeHold {
            address: a.into(),
            interface: lan.into(),
            ..Default::default()
        })
        .into()
}

/// The test's namespaces, wired: the client and the edge on one link, the edge and the node on
/// another, and the workload behind the node; none where the kernel makes no WireGuard or GRE
/// link.
fn network(id: u32) -> Option<(Netns, Netns, Netns, Netns)> {
    let named = |n: &str| Netns::new(format!("korpis-edg-{id}-{n}"));
    let (client, e, n, w) = (named("c"), named("e"), named("n"), named("w"));
    let lan = format!("kec{id}e");
    wire((&client, &format!("kec{id}c"), "198.18.1.2/24"), (&e, &lan, "198.18.1.1/24"));
    wire((&e, &format!("kem{id}e"), "198.18.0.1/30"), (&n, &format!("kem{id}n"), "198.18.0.2/30"));
    if !capable(&e) {
        eprintln!("this kernel makes no WireGuard or GRE link: not run");
        return None;
    }
    let link = format!("kw{id}n");
    wire((&n, &link, "10.48.4.1/30"), (&w, &format!("{link}w"), "10.48.4.2/30"));
    assert!(w.run(&["ip", "route", "add", "default", "via", "10.48.4.1"]));
    for ns in [&e, &n] {
        // The test's own namespaces forward, as nodes do, and check each packet's source in
        // strict mode, the strictest a host may ask for; the host's settings are left alone.
        assert!(ns.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
        assert!(ns.run(&["sysctl", "-qw", "net.ipv4.conf.all.rp_filter=1"]));
    }
    Some((client, e, n, w))
}

#[test]
fn a_client_reaches_a_workload_behind_an_edge_at_its_stable_address_and_keeps_its_own() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("edges need root: not run");
        return;
    }
    let id = std::process::id() % 100_000;
    let Some((client, e, n, w)) = network(id) else { return };
    let lan = format!("kec{id}e");
    let listeners: Vec<TcpListener> = [25_565, 25_567]
        .map(|p| w.within(move || TcpListener::bind(("10.48.4.2", p)).unwrap()))
        .into();

    let dir = tempfile::tempdir().unwrap();
    let (oe, on) = (Overlay::new(dir.path().join("e.key")), Overlay::new(dir.path().join("n.key")));
    let bare = || helper::KeepOverlayRequest { port: 51820, ..Default::default() };
    let (ke, kn) = (e.keep(&oe, bare()), n.keep(&on, bare()));
    // An address the edge's interface had already is the operator's.
    assert!(e.run(&["ip", "addr", "add", "198.18.1.11/32", "dev", &lan]));
    let refused = helper::EdgeTranslation {
        via: String::new(),
        to_address: String::new(),
        to_port: 0,
        ..translated(25_566)
    };
    let table = || {
        let udp = udp_translated(25_570, 25_570);
        vec![translated(25_565), refused.clone(), translated(25_567), udp]
    };
    n.keep(&on, node(&ke.public_key));
    stale_neighbour(&client, &format!("kec{id}c"));
    e.keep(&oe, edge(&kn.public_key, table(), held(&lan)));
    assert!(announced(&client, &e, &lan), "the address taken is announced on its link");

    let seen = listeners[0].try_clone().unwrap();
    let accepted = std::thread::spawn(move || seen.accept().map(|(_, from)| from.ip()));
    connect(&client, 25_565).expect("the stable address reaches the workload");
    let from = accepted.join().unwrap().unwrap();
    assert_eq!(from.to_string(), "198.18.1.2", "the workload sees the client's own address");

    let nothing = connect(&client, 25_566).unwrap_err();
    assert_eq!(nothing.kind(), ErrorKind::ConnectionRefused, "refused at once, never a black hole");
    let closed = connect(&client, 22).unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::ConnectionRefused, "a port no endpoint holds");
    assert!(connect(&client, 25_567).is_err(), "the node admits what it was told and no more");

    a_flow_follows_its_port(&client, &w, || {
        let mut moved = table();
        moved.pop();
        moved.push(udp_translated(25_570, 25_571));
        e.keep(&oe, edge(&kn.public_key, moved, held(&lan)));
    });
    outside_the_mesh(&client, &e, &n);
    drop(listeners);

    e.keep(&oe, edge(&kn.public_key, table(), Vec::new()));
    let shown =
        Command::new("ip").args(["-n", &e.0, "addr", "show", "dev", &lan]).output().unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(!shown.contains("198.18.1.10"), "no longer held");
    assert!(shown.contains("198.18.1.11"), "the operator's own address stays");
}
