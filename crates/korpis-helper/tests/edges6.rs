// SPDX-License-Identifier: AGPL-3.0-or-later

//! An edge forwarding an IPv6 stable address to a workload on another node (§3.1 of
//! docs/07-networking.md, trace 78 of docs/23-walkthroughs.md), as the helper keeps it on both.
//! The client and the edge share a link of IPv6 alone, the edge and the node are joined by the
//! mesh over IPv4, and the workload behind the node has a unique local address of the node's
//! prefix: the GRE link inside the mesh, IPv4 itself, carries IPv6. The address taken is
//! announced with an unsolicited neighbour advertisement; the client reaches the workload and the
//! workload sees the client's own address; a port no endpoint holds is refused at once; the
//! probe port reaches the node at its own IPv6 address in the mesh and is answered back through
//! the link; and the address is taken off when no longer held. Needs root, the kernel's
//! WireGuard and GRE, and nftables; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

mod common;

use common::{Netns, capable, wire};
use korpis_helper::overlay::Overlay;
use korpis_proto::korpis::helper::v1 as helper;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::Command;
use std::time::Duration;

const STABLE: &str = "2001:db8:1::10";
const CLIENT: &str = "2001:db8:1::2";
const WORKLOAD: &str = "fd00:0:0:4::2";
/// The node's own IPv6 address in the mesh, the last of its unique local prefix.
const NODE_MESH: &str = "fd00:0:0:4:ffff:ffff:ffff:ffff";
const PROBE: u16 = 65_535;

/// A link of IPv6 alone between `a` and `b`, each end addressed without duplicate detection.
fn wire6(a: (&Netns, &str, &str), b: (&Netns, &str, &str)) {
    assert!(common::ip(&[
        "link", "add", a.1, "netns", &a.0.0, "type", "veth", "peer", "name", b.1, "netns", &b.0.0
    ]));
    for (ns, link, addr) in [a, b] {
        assert!(ns.run(&["ip", "addr", "add", addr, "dev", link, "nodad"]));
        assert!(ns.run(&["ip", "link", "set", link, "up"]));
    }
}

fn translation(
    port: u16,
    transport: helper::Transport,
    to: (&str, u16),
) -> helper::EdgeTranslation {
    helper::EdgeTranslation {
        address: STABLE.into(),
        port: u32::from(port),
        transport: transport.into(),
        via: "10.48.7.255".into(),
        to_address: to.0.into(),
        to_port: u32::from(to.1),
        ..Default::default()
    }
}

/// The edge's request: its peer the node, the game's port and the probe's translated into the
/// node's link, and `holds`.
fn edge(node_key: &[u8], holds: Vec<helper::EdgeHold>) -> helper::KeepOverlayRequest {
    let tcp = helper::Transport::TRANSPORT_TCP;
    let udp = helper::Transport::TRANSPORT_UDP;
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
            translations: vec![
                translation(25_565, tcp, (WORKLOAD, 25_565)),
                translation(PROBE, udp, (NODE_MESH, PROBE)),
            ],
            holds,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// The node's request: its peer the edge, its IPv6 address in the mesh, and the edge admitted to
/// the workload's 25565 and to the probe at that address.
fn node(edge_key: &[u8]) -> helper::KeepOverlayRequest {
    let admit = |destination: &str, port: u16, transport: helper::Transport| helper::EdgeAdmit {
        source: "10.48.3.255".into(),
        destination: destination.into(),
        port: u32::from(port),
        transport: transport.into(),
        ..Default::default()
    };
    helper::KeepOverlayRequest {
        port: 51820,
        peers: vec![helper::OverlayPeer {
            public_key: edge_key.to_vec(),
            endpoint: "198.18.0.1:51820".into(),
            allowed_ips: vec!["10.48.0.0/22".into()],
            ..Default::default()
        }],
        mesh_address: "10.48.7.255".into(),
        mesh_address_v6: NODE_MESH.into(),
        edges: helper::EdgeTable {
            tunnels: vec!["10.48.3.255".into()],
            admits: vec![
                admit(WORKLOAD, 25_565, helper::Transport::TRANSPORT_TCP),
                admit(NODE_MESH, PROBE, helper::Transport::TRANSPORT_UDP),
            ],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn held(lan: &str) -> Vec<helper::EdgeHold> {
    vec![helper::EdgeHold { address: STABLE.into(), interface: lan.into(), ..Default::default() }]
}

/// What the client's connection to the stable address on `port` comes to, within two seconds.
fn connect(client: &Netns, port: u16) -> std::io::Result<()> {
    let to = SocketAddr::new(STABLE.parse().map_err(std::io::Error::other)?, port);
    client.within(move || TcpStream::connect_timeout(&to, Duration::from_secs(2)).map(|_| ()))
}

/// The hardware address `ns` shows of `args`, `ip`'s brief listing of a link or a neighbour.
fn shown(ns: &Netns, args: &[&str]) -> String {
    let all = [&["-n", &ns.0][..], args].concat();
    String::from_utf8_lossy(&Command::new("ip").args(&all).output().unwrap().stdout).into_owned()
}

/// The client's entry for the stable address naming another machine, as after its last holder;
/// whether, once the edge has taken the address, it names the edge's link `lan`.
fn announced(
    client: &Netns,
    e: &Netns,
    (lan, client_link): (&str, &str),
    take: impl FnOnce(),
) -> bool {
    let fake = ["lladdr", "02:00:00:00:00:99", "nud", "stale", "dev", client_link];
    assert!(client.run(&[&["ip", "-6", "neigh", "replace", STABLE][..], &fake].concat()));
    take();
    let ours = shown(e, &["-br", "link", "show", "dev", lan]);
    let mac = ours.split_whitespace().nth(2).unwrap().to_owned();
    std::thread::sleep(Duration::from_millis(200));
    shown(client, &["-6", "neigh", "show", STABLE]).contains(&mac)
}

/// The probe sent to the stable address's probe port reaches the node at its IPv6 mesh address,
/// whose answer comes back through the link and the edge's translation, from the stable address.
fn probed(client: &Netns, n: &Netns) -> bool {
    let answerer = n.within(|| UdpSocket::bind((NODE_MESH, PROBE)).unwrap());
    let echo = std::thread::spawn(move || {
        answerer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut buffer = [0; 16];
        if let Ok((len, from)) = answerer.recv_from(&mut buffer) {
            let _ = answerer.send_to(buffer.get(..len).unwrap(), from);
        }
    });
    let prober = client.within(|| UdpSocket::bind((CLIENT, 0)).unwrap());
    prober.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    prober.send_to(b"probe", (STABLE, PROBE)).unwrap();
    let mut answer = [0; 16];
    let got = prober.recv_from(&mut answer);
    echo.join().unwrap();
    got.is_ok_and(|(len, from)| {
        answer.get(..len) == Some(&b"probe"[..])
            && from == SocketAddr::new(STABLE.parse().unwrap(), PROBE)
    })
}

/// Waits, five seconds at most, until no address of `ns` is still tentative: a link made a
/// moment ago has its link-local address under duplicate detection, and until that ends the
/// kernel leaves a neighbour's solicitation on the link unanswered now and then.
fn settled(ns: &Netns) {
    for _ in 0..50 {
        if shown(ns, &["-6", "addr", "show", "tentative"]).trim().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The test's namespaces: the client and the edge on a link of IPv6 alone, the edge and the node
/// on an IPv4 link the mesh crosses, and the workload behind the node; none where the kernel
/// makes no WireGuard or GRE link.
fn network(id: u32) -> Option<(Netns, Netns, Netns, Netns)> {
    let named = |n: &str| Netns::new(format!("korpis-ed6-{id}-{n}"));
    let (client, e, n, w) = (named("c"), named("e"), named("n"), named("w"));
    wire6(
        (&client, &format!("k6c{id}c"), "2001:db8:1::2/64"),
        (&e, &format!("k6c{id}e"), "2001:db8:1::1/64"),
    );
    wire((&e, &format!("k6m{id}e"), "198.18.0.1/30"), (&n, &format!("k6m{id}n"), "198.18.0.2/30"));
    if !capable(&e) {
        eprintln!("this kernel makes no WireGuard or GRE link: not run");
        return None;
    }
    let link = format!("k6w{id}n");
    wire6((&n, &link, "fd00:0:0:4::1/64"), (&w, &format!("{link}w"), "fd00:0:0:4::2/64"));
    assert!(w.run(&["ip", "-6", "route", "add", "default", "via", "fd00:0:0:4::1"]));
    for ns in [&client, &e, &n, &w] {
        settled(ns);
    }
    for ns in [&e, &n] {
        // The test's own namespaces forward, as nodes do; the host's settings are left alone.
        assert!(ns.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
        assert!(ns.run(&["sysctl", "-qw", "net.ipv6.conf.all.forwarding=1"]));
    }
    Some((client, e, n, w))
}

#[test]
fn a_client_reaches_a_workload_at_an_ipv6_stable_address_through_the_ipv4_mesh() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("edges need root: not run");
        return;
    }
    let id = std::process::id() % 100_000;
    let Some((client, e, n, w)) = network(id) else { return };
    let lan = format!("k6c{id}e");
    let listener = w.within(|| TcpListener::bind((WORKLOAD, 25_565)).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let (oe, on) = (Overlay::new(dir.path().join("e.key")), Overlay::new(dir.path().join("n.key")));
    let bare = || helper::KeepOverlayRequest { port: 51820, ..Default::default() };
    let (ke, kn) = (e.keep(&oe, bare()), n.keep(&on, bare()));
    n.keep(&on, node(&ke.public_key));
    let taken = || {
        e.keep(&oe, edge(&kn.public_key, held(&lan)));
    };
    assert!(announced(&client, &e, (&lan, &format!("k6c{id}c")), taken), "announced on its link");

    let accepted = std::thread::spawn(move || listener.accept().map(|(_, from)| from.ip()));
    connect(&client, 25_565).expect("the stable address reaches the workload over IPv6");
    let from: IpAddr = accepted.join().unwrap().unwrap();
    assert_eq!(from.to_string(), CLIENT, "the workload sees the client's own address");
    let closed = connect(&client, 22).unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::ConnectionRefused, "a port no endpoint holds");
    assert!(probed(&client, &n), "the probe crosses to the node's mesh address and back");

    e.keep(&oe, edge(&kn.public_key, Vec::new()));
    assert!(!shown(&e, &["addr", "show", "dev", &lan]).contains(STABLE), "no longer held");
}
