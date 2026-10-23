// SPDX-License-Identifier: AGPL-3.0-or-later

//! The socket a health probe is made from (§5 of docs/04-runtimes.md): opened by the helper
//! inside the workload's namespace, it reaches the workload's endpoint where the endpoint's own
//! rules turn away every connection from elsewhere, the host's among them. Needs root and
//! nftables; skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_helper::network::Network;
use korpis_proto::korpis::helper::v1 as helper;
use std::io::{BufRead as _, BufReader, Read as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A workload's network, released however the test ends.
struct Held(Network, String);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.release(&self.1);
    }
}

/// A process, killed however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A listener on `at` inside the namespace bound at `net`, which answers its first connection
/// with "probed", once it listens.
fn listening(net: &std::path::Path, at: SocketAddrV4) -> Running {
    let script = format!(
        "import socket\ns=socket.socket()\ns.bind(('{}',{}))\ns.listen()\nprint('up',flush=True)\n\
         c,_=s.accept()\nc.sendall(b'probed')\nc.close()",
        at.ip(),
        at.port()
    );
    let mut child = Command::new("nsenter")
        .arg(format!("--net={}", net.display()))
        .args(["python3", "-c", &script])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "up");
    Running(child)
}

#[test]
fn a_probe_s_socket_reaches_an_endpoint_whose_rules_turn_the_host_away() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let network = Network::new(tmp.path().join("netns"));
    if !rustix::process::geteuid().is_root() || !network.describe().offered {
        eprintln!("networks need root and a node that offers them: not run");
        return;
    }
    let workload = format!("probe-test-{}", std::process::id());
    let ip = Ipv4Addr::new(10, 243, u8::try_from(std::process::id() % 250).unwrap(), 2);
    // Every address, in the two halves a rule can name.
    let deny = |cidr: &str| helper::Rule { allow: false, cidr: cidr.into(), ..Default::default() };
    let port = helper::Port {
        transport: helper::Transport::TRANSPORT_TCP.into(),
        port: 7002,
        rules: vec![deny("0.0.0.0/1"), deny("128.0.0.0/1")],
        ..Default::default()
    };
    let request = helper::EnsureNetworkRequest {
        workload: workload.clone(),
        ipv4: ip.to_string(),
        ipv4_pool: "10.243.0.0/16".into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        inbound: vec![port],
        ..Default::default()
    };
    let made = network.ensure(&request).unwrap();
    let _held = Held(network.clone(), workload.clone());
    let at = SocketAddrV4::new(ip, 7002);
    let _listener = listening(&std::path::Path::new(&made.namespaces).join("net"), at);

    let from_host = TcpStream::connect_timeout(&SocketAddr::V4(at), Duration::from_secs(2));
    assert!(from_host.is_err(), "the endpoint's rule turns the host away");
    let mut sockets = network.open_probe(&workload).unwrap();
    assert_eq!(sockets.len(), 1);
    let socket = sockets.remove(0);
    rustix::net::connect(&socket, &at).expect("the probe's socket reaches it from inside");
    let mut said = String::new();
    TcpStream::from(socket).read_to_string(&mut said).unwrap();
    assert_eq!(said, "probed");
    assert!(network.open_probe("no-such-workload").is_err(), "a workload with no network");
}
