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
