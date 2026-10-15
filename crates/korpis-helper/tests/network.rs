// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, made by the helper on this machine (§4.1 of docs/07-networking.md):
//! its namespace and routed link, its addresses, its closed inbound, and its removal. Needs root
//! and nftables; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_helper::network::{Network, host_link};
use korpis_proto::korpis::helper::v1 as helper;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn able(network: &Network) -> bool {
    if !rustix::process::geteuid().is_root() {
        eprintln!("networks need root: not run");
        return false;
    }
    let described = network.describe();
    if !described.offered {
        eprintln!("this machine offers no network ({}): not run", described.why_not);
    }
    described.offered
}

fn request(workload: &str, ipv4: Ipv4Addr) -> helper::EnsureNetworkRequest {
    helper::EnsureNetworkRequest {
        workload: workload.into(),
        ipv4: ipv4.to_string(),
        ipv4_pool: "10.246.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        ..Default::default()
    }
}

/// Runs `args` inside the namespace bound at `path`; whether it succeeded.
fn inside(path: &Path, args: &[&str]) -> bool {
    let net = format!("--net={}", path.display());
    Command::new("nsenter").arg(net).args(args).output().is_ok_and(|o| o.status.success())
}

fn link_exists(name: &str) -> bool {
    Command::new("ip").args(["link", "show", name]).output().is_ok_and(|o| o.status.success())
}

#[test]
fn a_workload_gets_a_routed_namespace_closed_to_new_connections_and_loses_it_on_release() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    if !able(&network) {
        return;
    }
    let workload = format!("net-test-{}", std::process::id());
    let ip = Ipv4Addr::new(10, 246, u8::try_from(std::process::id() % 250).unwrap(), 2);
    let made = network.ensure(&request(&workload, ip)).unwrap();
    assert_eq!(made.host_link, host_link(&workload));
    assert!(link_exists(&made.host_link));
    let dir = Path::new(&made.namespaces).to_owned();
    let path = dir.join("net");
    let user = format!("--user={}", dir.join("user").display());
    let map = Command::new("nsenter").args([&user, "cat", "/proc/self/uid_map"]).output().unwrap();
    let map = String::from_utf8_lossy(&map.stdout).split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(map, format!("0 {FIRST_WORKLOAD_ID} 65536"), "the network's owner maps the range");
    assert!(inside(&path, &["ip", "-4", "addr", "show", "dev", "eth0", "to", &format!("{ip}/32")]));

    // The host reaches the workload, and the workload its gateway, over ICMP.
    let ping = Command::new("ping").args(["-c1", "-W2", &ip.to_string()]).output().unwrap();
    assert!(ping.status.success());
    assert!(inside(&path, &["ping", "-c1", "-W2", "169.254.1.1"]));

    // A port the workload listens on, and no endpoint declares, is closed to the host.
    let mut listener = Command::new("nsenter")
        .arg(format!("--net={}", path.display()))
        .args(["python3", "-c", "import socket,time;s=socket.socket();s.bind(('0.0.0.0',7001));s.listen();time.sleep(10)"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let connect = format!("import socket;socket.create_connection(('{ip}',7001),2)");
    assert!(inside(&path, &["python3", "-c", &connect]), "the workload reaches itself");
    let reached = TcpStream::connect_timeout(&SocketAddr::from((ip, 7001)), Duration::from_secs(1));
    let _ = listener.kill();
    let _ = listener.wait();
    assert!(reached.is_err(), "a port no endpoint declares must not be reached");

    // Asked again, nothing changes and nothing fails.
    assert_eq!(network.ensure(&request(&workload, ip)).unwrap(), made);

    assert!(network.release(&workload).unwrap());
    assert!(!link_exists(&made.host_link));
    assert!(!dir.exists());
    assert!(!network.release(&workload).unwrap(), "a second release finds nothing");
}

#[test]
fn an_address_outside_its_pool_is_refused_before_anything_is_made() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    let workload = format!("net-refused-{}", std::process::id());
    let refused = network.ensure(&request(&workload, Ipv4Addr::new(192, 168, 7, 2)));
    assert!(refused.is_err());
    assert!(!tmp.path().join("netns").join(&workload).exists());
    assert!(!link_exists(&host_link(&workload)));
}

/// An address of this machine that is not loopback: the one its traffic towards the internet
/// leaves from.
fn host_address() -> Option<Ipv4Addr> {
    let out = Command::new("ip").args(["-4", "-o", "route", "get", "1.1.1.1"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut words = text.split_whitespace();
    words.find(|w| *w == "src")?;
    words.next()?.parse().ok()
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

#[test]
fn a_node_port_reaches_the_declared_port_and_nothing_else() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    let Some(node) = host_address().filter(|_| able(&network)) else { return };
    let workload = format!("net-fwd-{}", std::process::id());
    let ip = Ipv4Addr::new(10, 245, u8::try_from(std::process::id() % 250).unwrap(), 2);
    let node_port = u16::try_from(31000 + std::process::id() % 1000).unwrap();
    let tcp = helper::Transport::TRANSPORT_TCP;
    let mut r = request(&workload, ip);
    r.ipv4_pool = "10.245.0.0/16".into();
    r.inbound = vec![helper::Port { transport: tcp.into(), port: 7001, ..Default::default() }];
    r.forwards = vec![helper::Forward {
        transport: tcp.into(),
        node_port: u32::from(node_port),
        container_port: 7001,
        ..Default::default()
    }];
    let made = network.ensure(&r).unwrap();
    let path = Path::new(&made.namespaces).join("net");
    let (mut declared, mut other) = (listen(&path, 7001), listen(&path, 7002));
    std::thread::sleep(Duration::from_millis(500));
    let at = |addr: Ipv4Addr, port: u16| {
        TcpStream::connect_timeout(&SocketAddr::from((addr, port)), Duration::from_secs(1)).is_ok()
    };
    let forwarded = at(node, node_port);
    let direct = at(ip, 7001);
    let undeclared = at(ip, 7002);
    for child in [&mut declared, &mut other] {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(forwarded, "the node's port reaches the declared port");
    assert!(direct, "the declared port is open");
    assert!(!undeclared, "a port no endpoint declares is closed");

    assert!(network.release(&workload).unwrap());
    let listed = Command::new("nft").args(["list", "map", "inet", "korpis", "forwarded"]).output();
    let listed = String::from_utf8_lossy(&listed.unwrap().stdout).into_owned();
    assert!(!listed.contains(&node_port.to_string()), "released: {listed}");
}

/// What the rows of the host's table commented `comment` have counted, in packets.
fn counted(comment: &str) -> u64 {
    let out = Command::new("nft").args(["-j", "list", "table", "inet", "korpis"]).output().unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
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

/// Tries a TCP connection from inside the namespace at `path` to `to`, for a second.
fn knock(path: &Path, to: &str, port: u16) {
    let script = format!(
        "import socket\ntry: socket.create_connection(('{to}',{port}),1)\nexcept Exception: pass"
    );
    inside(path, &["python3", "-c", &script]);
}

#[test]
fn each_row_of_the_egress_default_drops_what_it_names_and_counts_it() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    let Some(node) = host_address().filter(|_| able(&network)) else { return };
    let pid = std::process::id();
    let third = u8::try_from(pid % 250).unwrap();
    let (a, b) = (format!("net-eg-a-{pid}"), format!("net-eg-b-{pid}"));
    let ip_b = Ipv4Addr::new(10, 244, third, 3);
    let mut ra = request(&a, Ipv4Addr::new(10, 244, third, 2));
    ra.ipv4_pool = "10.244.0.0/16".into();
    // The control plane's API, opened for this workload as for an installed extension's.
    ra.opened = vec![helper::Destination {
        address: node.to_string(),
        port: 7443,
        transport: helper::Transport::TRANSPORT_TCP.into(),
        reason: helper::DestinationReason::DESTINATION_REASON_CONTROL_PLANE.into(),
        ..Default::default()
    }];
    let mut rb = request(&b, ip_b);
    rb.ipv4_pool = "10.244.0.0/16".into();
    rb.uid_base = FIRST_WORKLOAD_ID + (1 << 16);
    let path = Path::new(&network.ensure(&ra).unwrap().namespaces).join("net");
    network.ensure(&rb).unwrap();

    let rows = [
        ("other workloads", ip_b.to_string(), 7001),
        ("metadata", "169.254.169.254".to_owned(), 80),
        ("private", "192.168.77.1".to_owned(), 80),
        ("smtp", "203.0.113.5".to_owned(), 25),
        ("node", node.to_string(), 22),
        ("control plane", node.to_string(), 7443),
    ];
    let mut missed = Vec::new();
    for (row, to, port) in rows {
        let before = counted(row);
        knock(&path, &to, port);
        if counted(row) <= before {
            missed.push(row);
        }
    }
    // Past its rate, a workload's new connections are dropped.
    let flood = "import socket\nfor _ in range(600):\n s=socket.socket();s.setblocking(False)\n \
                 try: s.connect(('203.0.113.5',80))\n except Exception: pass";
    let before = counted("rate");
    inside(&path, &["python3", "-c", flood]);
    if counted("rate") <= before {
        missed.push("rate");
    }
    network.release(&a).unwrap();
    network.release(&b).unwrap();
    assert!(missed.is_empty(), "rows that counted nothing: {missed:?}");
}

/// What the rules commented `comment` in the namespace at `path` have counted, in packets.
fn counted_inside(path: &Path, comment: &str) -> u64 {
    let out = Command::new("nsenter")
        .arg(format!("--net={}", path.display()))
        .args(["nft", "-j", "list", "table", "inet", "korpis"])
        .output()
        .unwrap();
    let listing: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
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

fn cidr_rule(allow: bool, cidr: &str) -> helper::Rule {
    helper::Rule { allow, cidr: cidr.into(), ..Default::default() }
}

#[test]
fn a_workload_s_own_policy_holds_before_the_default_and_never_opens_what_it_protects() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    let Some(node) = host_address().filter(|_| able(&network)) else { return };
    let pid = std::process::id();
    let workload = format!("net-own-{pid}");
    let ip = Ipv4Addr::new(10, 243, u8::try_from(pid % 250).unwrap(), 2);
    let mut r = request(&workload, ip);
    r.ipv4_pool = "10.243.0.0/16".into();
    r.egress = vec![cidr_rule(false, "198.51.100.0/24"), cidr_rule(true, "192.168.0.0/16")];
    r.new_per_sec = 5;
    let tcp = helper::Transport::TRANSPORT_TCP;
    r.inbound = vec![helper::Port {
        transport: tcp.into(),
        port: 7001,
        rules: vec![cidr_rule(false, &format!("{node}/32"))],
        ..Default::default()
    }];
    let node_port = u16::try_from(32000 + pid % 1000).unwrap();
    r.forwards = vec![helper::Forward {
        transport: tcp.into(),
        node_port: u32::from(node_port),
        container_port: 7001,
        ..Default::default()
    }];
    let path = Path::new(&network.ensure(&r).unwrap().namespaces).join("net");

    let mut missed = Vec::new();
    for (row, to) in [("policy", "198.51.100.7"), ("private", "192.168.77.1")] {
        let before = counted(row);
        knock(&path, to, 80);
        if counted(row) <= before {
            missed.push(row);
        }
    }
    let flood = "import socket\nfor _ in range(60):\n s=socket.socket();s.setblocking(False)\n \
                 try: s.connect(('203.0.113.5',80))\n except Exception: pass";
    let before = counted("rate");
    inside(&path, &["python3", "-c", flood]);
    if counted("rate") <= before {
        missed.push("its own rate");
    }
    // A client at the node's own address, which its forwarded port preserves, is denied by the
    // endpoint's rule inside the namespace.
    let mut listener = listen(&path, 7001);
    std::thread::sleep(Duration::from_millis(500));
    let at = SocketAddr::from((node, node_port));
    let reached = TcpStream::connect_timeout(&at, Duration::from_secs(1));
    let _ = listener.kill();
    let _ = listener.wait();
    if reached.is_ok() || counted_inside(&path, "policy") == 0 {
        missed.push("the endpoint's own deny");
    }
    network.release(&workload).unwrap();
    assert!(missed.is_empty(), "what did not hold: {missed:?}");
}

#[test]
fn a_workload_s_own_limits_may_not_raise_the_default() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    let workload = format!("net-raised-{}", std::process::id());
    let mut r = request(&workload, Ipv4Addr::new(10, 246, 1, 9));
    r.max_connections = 1_000_000;
    assert!(network.ensure(&r).is_err());
    assert!(!tmp.path().join("netns").join(&workload).exists());
}

#[test]
fn a_workload_s_resolver_is_sockets_of_its_own_namespace_its_resolv_conf_names() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    if !able(&network) {
        return;
    }
    let workload = format!("net-dns-{}", std::process::id());
    let ip = Ipv4Addr::new(10, 242, u8::try_from(std::process::id() % 250).unwrap(), 2);
    let mut r = request(&workload, ip);
    r.ipv4_pool = "10.242.0.0/16".into();
    let dir = Path::new(&network.ensure(&r).unwrap().namespaces).to_owned();
    let conf = std::fs::read_to_string(dir.join("resolv.conf")).unwrap();
    assert_eq!(conf, "nameserver 169.254.1.53\n");

    let mut fds = network.open_resolver(&workload).unwrap().into_iter();
    let udp = std::net::UdpSocket::from(fds.next().unwrap());
    let tcp = std::net::TcpListener::from(fds.next().unwrap());
    udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let send = "import socket;socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\
                .sendto(b'asked',('169.254.1.53',53))";
    assert!(inside(&dir.join("net"), &["python3", "-c", send]));
    let mut buf = [0u8; 16];
    // Sent within the namespace, from the resolver's own address: the socket is the workload.
    let (n, _) = udp.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"asked", "what the workload sent its resolver");
    let connect = "import socket;socket.create_connection(('169.254.1.53',53),2)";
    assert!(inside(&dir.join("net"), &["python3", "-c", connect]), "TCP listens");
    drop((udp, tcp));
    network.release(&workload).unwrap();
    assert!(network.open_resolver(&workload).is_err(), "no network, no resolver");
}
