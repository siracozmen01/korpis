// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's bandwidth, shaped by the helper on this machine (§7 of docs/07-networking.md):
//! an uplink's tree, the workload's class on it and the forwarded packets it classes, the limits
//! on its link and its endpoint's own classes there, the counters of its endpoint and of its
//! egress, and their removal. The test runs
//! in a network namespace of its own, standing for the host, whose uplink is a dummy interface:
//! nothing the machine uses is shaped or rewritten, and no other firewall on it drops what is
//! forwarded. Needs root, nftables, and tc; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_helper::network::{Network, host_link};
use korpis_proto::korpis::helper::v1 as helper;
use serde_json::Value;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn able(network: &Network) -> bool {
    if !rustix::process::geteuid().is_root() {
        eprintln!("shaping needs root: not run");
        return false;
    }
    let d = network.describe();
    if !d.offered || !d.shapes {
        eprintln!("this machine shapes nothing ({}{}): not run", d.why_not, d.why_not_shapes);
        return false;
    }
    true
}

fn json(args: &[&str]) -> Value {
    let out = Command::new(args[0]).args(&args[1..]).output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

/// A rate as `tc` prints one, `1Gbit` or `6250Kbit`, in bits a second.
fn bits(text: &str) -> u64 {
    let units = [("Tbit", 1e12), ("Gbit", 1e9), ("Mbit", 1e6), ("Kbit", 1e3), ("bit", 1.0)];
    units
        .into_iter()
        .find_map(|(unit, factor)| Some(text.strip_suffix(unit)?.parse::<f64>().ok()? * factor))
        .map_or(0, |b| b.round() as u64)
}

/// The classes of `dev`, by handle: rate and ceiling in bits a second, and bytes sent. `tc`
/// lists an HTB class only as text, whatever `-j` asks for.
fn classes(dev: &str) -> Vec<(String, u64, u64, u64)> {
    classes_in(&Command::new("tc").args(["-s", "class", "show", "dev", dev]).output().unwrap())
}

fn classes_in(out: &std::process::Output) -> Vec<(String, u64, u64, u64)> {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut found = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() != Some(&"class") {
            continue;
        }
        let after = |w: &str| words.iter().position(|x| *x == w).and_then(|i| words.get(i + 1));
        let sent = lines
            .peek()
            .map_or(0, |l| l.split_whitespace().nth(1).and_then(|n| n.parse().ok()).unwrap_or(0));
        let (rate, ceil) =
            (after("rate").map_or(0, |r| bits(r)), after("ceil").map_or(0, |c| bits(c)));
        found.push(((*words.get(2).unwrap_or(&"")).to_owned(), rate, ceil, sent));
    }
    found
}

fn rate_of(dev: &str, handle: &str) -> Option<u64> {
    classes(dev).into_iter().find(|c| c.0 == handle).map(|c| c.1)
}

/// The counters of the host's table, by name: bytes and packets.
fn counters() -> Vec<(String, u64, u64)> {
    let listed = json(&["nft", "-j", "list", "counters", "table", "inet", "korpis"]);
    let items = listed.get("nftables").and_then(Value::as_array).cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i.get("counter"))
        .map(|c| {
            let n = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
            let name = c.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
            (name, n("bytes"), n("packets"))
        })
        .collect()
}

fn counted(name: &str) -> Option<(u64, u64)> {
    counters().into_iter().find(|c| c.0 == name).map(|c| (c.1, c.2))
}

fn inside(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("nsenter");
    c.arg(format!("--net={}", dir.join("net").display())).args(args);
    c
}

fn request(workload: &str, ipv4: Ipv4Addr, reserved: u64) -> helper::EnsureNetworkRequest {
    let port = helper::Port {
        transport: helper::Transport::TRANSPORT_TCP.into(),
        port: 8080,
        egress_limit: 5_000_000,
        ingress_limit: 3_000_000,
        accounted: true,
        ..Default::default()
    };
    let unaccounted = helper::Port {
        transport: helper::Transport::TRANSPORT_TCP.into(),
        port: 9090,
        ..Default::default()
    };
    helper::EnsureNetworkRequest {
        workload: workload.into(),
        ipv4: ipv4.to_string(),
        ipv4_pool: "10.247.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        inbound: vec![port, unaccounted],
        bandwidth: helper::Bandwidth {
            egress_reservation: reserved,
            egress_limit: 200_000_000,
            ingress_limit: 50_000_000,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// The network namespace the test stands in for the host, removed when it ends.
struct Host(String);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).output();
    }
}

/// Moves the calling thread into a fresh network namespace, with its loopback up and IPv4
/// forwarding on, as a node's host is.
fn host(pid: u32) -> Host {
    use rustix::thread::{LinkNameSpaceType, move_into_link_name_space};
    use std::os::fd::AsFd as _;
    let name = format!("kpbw{pid}");
    let made = Command::new("ip").args(["netns", "add", &name]).output();
    assert!(made.is_ok_and(|o| o.status.success()), "a namespace for the host");
    let ns = std::fs::File::open(format!("/run/netns/{name}")).unwrap();
    move_into_link_name_space(ns.as_fd(), Some(LinkNameSpaceType::Network)).unwrap();
    Command::new("ip").args(["link", "set", "lo", "up"]).output().unwrap();
    std::fs::write("/proc/sys/net/ipv4/ip_forward", "1").unwrap();
    Host(name)
}

/// The workload's network, released however the test ends.
struct Made<'a>(&'a Network, String);

impl Drop for Made<'_> {
    fn drop(&mut self) {
        let _ = self.0.release(&self.1);
    }
}

/// The dummy interface standing for an uplink, made for the test and removed after it.
struct Dummy(String);

impl Drop for Dummy {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).output();
    }
}

/// `dev` declared as the node's uplink, a gigabit of which the node keeps 50 Mbit.
fn declared(dev: &str) -> helper::EnsureUplinksRequest {
    let uplink = helper::Uplink {
        interface: dev.to_owned(),
        capacity: 1_000_000_000,
        reserve: 50_000_000,
        ..Default::default()
    };
    helper::EnsureUplinksRequest { uplinks: vec![uplink], ..Default::default() }
}

/// The workload's `link` is never taken for an uplink: its network made again, the link holds no
/// class of the uplinks', and the uplinks made again leave its limits as they were.
fn kept_apart(network: &Network, r: &helper::EnsureNetworkRequest, (link, dev): (&str, &str)) {
    network.ensure(r).unwrap();
    assert!(!classes(link).iter().any(|c| c.0.starts_with("4b50:")), "{:?}", classes(link));
    let again = network.ensure_uplinks(&declared(dev)).unwrap();
    assert!(again.removed.is_empty(), "a workload's link is not an uplink: {again:?}");
    assert_eq!(rate_of(link, "4b51:1"), Some(50_000_000), "its ingress limit is kept");
}

/// A dummy interface, declared to `network` as the node's uplink and shaped.
fn uplink(network: &Network, pid: u32) -> Dummy {
    let dummy = Dummy(format!("kpup{}", pid % 100_000));
    let made = Command::new("ip").args(["link", "add", &dummy.0, "type", "dummy"]).output();
    assert!(made.is_ok_and(|o| o.status.success()), "a dummy uplink");
    Command::new("ip").args(["link", "set", &dummy.0, "up"]).output().unwrap();
    assert_eq!(network.ensure_uplinks(&declared(&dummy.0)).unwrap().shaped, vec![dummy.0.clone()]);
    assert_eq!(rate_of(&dummy.0, "4b50:1"), Some(1_000_000_000), "the uplink's capacity");
    assert_eq!(rate_of(&dummy.0, "4b50:2"), Some(50_000_000), "what the node keeps");
    dummy
}

/// The limits on `link` and inside `dir`, its endpoint's own classes, and a connection to it
/// at `ip` counted and classed.
fn endpoint(link: &str, dir: &Path, ip: Ipv4Addr) {
    assert_eq!(rate_of(link, "4b51:1"), Some(50_000_000), "its ingress limit, on the host's side");
    let eth0 = || {
        classes_in(&inside(dir, &["tc", "-s", "class", "show", "dev", "eth0"]).output().unwrap())
    };
    let find = |listed: Vec<(String, u64, u64, u64)>, handle: &str| {
        listed.into_iter().find(|c| c.0 == handle)
    };
    let root = find(eth0(), "4b51:1").map(|c| c.1);
    assert_eq!(root, Some(200_000_000), "its limit, on its own link");
    let (sent, received) = (find(eth0(), "4b51:10"), find(classes(link), "4b51:10"));
    assert_eq!(sent.map(|c| c.2), Some(5_000_000), "its endpoint's own limit, sent");
    assert_eq!(received.map(|c| c.2), Some(3_000_000), "and sent to it");

    // A connection to its endpoint is counted in, and goes in the endpoint's class on the host's
    // side of the link, as the reset it is answered with does on its own; what it sends to the
    // node is counted as egress. A port whose accounting is off is not counted.
    let to = SocketAddr::from((ip, 8080));
    let _ = TcpStream::connect_timeout(&to, Duration::from_secs(1));
    assert!(counted(&format!("{link}_tcp_8080_in")).is_some_and(|(_, p)| p > 0));
    assert!(find(classes(link), "4b51:10").is_some_and(|c| c.3 > 0), "{:?}", classes(link));
    assert!(find(eth0(), "4b51:10").is_some_and(|c| c.3 > 0), "{:?}", eth0());
    assert!(counted(&format!("{link}_tcp_9090_in")).is_none(), "not counted");
}

#[test]
fn a_reservation_is_a_class_on_the_uplink_and_each_endpoint_is_counted_until_released() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    if !rustix::process::geteuid().is_root() {
        eprintln!("shaping needs root: not run");
        return;
    }
    let pid = std::process::id();
    let _host = host(pid);
    if !able(&network) {
        return;
    }
    let dummy = uplink(&network, pid);

    let workload = format!("bw-test-{pid}");
    let _made = Made(&network, workload.clone());
    let ip = Ipv4Addr::new(10, 247, u8::try_from(pid % 250).unwrap(), 2);
    let made = network.ensure(&request(&workload, ip, 100_000_000)).unwrap();
    let (link, dir) = (host_link(&workload), Path::new(&made.namespaces).to_owned());
    let class =
        classes(&dummy.0).into_iter().find(|c| c.0.starts_with("4b50:1") && c.0 != "4b50:1");
    let class = class.expect("the workload's class on the uplink");
    assert_eq!((class.1, class.2), (100_000_000, 200_000_000), "its reservation, its limit");
    endpoint(&link, &dir, ip);
    kept_apart(&network, &request(&workload, ip, 100_000_000), (&link, &dummy.0));
    assert!(
        inside(&dir, &["ping", "-c1", "-W2", "169.254.1.1"]).output().unwrap().status.success()
    );
    assert!(counted(&format!("{link}_egress")).is_some_and(|(b, _)| b > 0));
    let read = network.read_counters().unwrap().counters;
    assert!(
        read.iter().any(|c| c.name == format!("{link}_tcp_8080_in") && c.packets > 0),
        "{read:?}"
    );

    // What it sends on through the uplink goes in its class, by the priority the host table
    // sets as it is forwarded.
    let route = ["route", "add", "198.51.100.0/24", "dev", &dummy.0];
    assert!(Command::new("ip").args(route).output().unwrap().status.success());
    let _ = inside(&dir, &["ping", "-c3", "-i0.2", "-W1", "198.51.100.7"]).output();
    let sent = classes(&dummy.0).into_iter().find(|c| c.0 == class.0).map_or(0, |c| c.3);
    assert!(sent > 0, "forwarded packets are classed: {:?}", classes(&dummy.0));

    // Reserving nothing, it has no class; released, nothing of it is left.
    network.ensure(&request(&workload, ip, 0)).unwrap();
    assert!(!classes(&dummy.0).iter().any(|c| c.0 == class.0), "its class is gone");
    network.release(&workload).unwrap();
    assert!(!counters().iter().any(|c| c.0.starts_with(&format!("{link}_"))), "its counters");
    let none = helper::EnsureUplinksRequest::default();
    assert_eq!(network.ensure_uplinks(&none).unwrap().removed, vec![dummy.0.clone()]);
    assert!(classes(&dummy.0).is_empty(), "an uplink no longer declared is not shaped");
}
