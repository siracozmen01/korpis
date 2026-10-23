// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's network (§4.1 of docs/07-networking.md): the helper makes the workload's namespace
//! with a tap beside its link, the jailed Firecracker joins it, and the guest's own kernel holds
//! the workload's address. The guest reaches its resolver's address in the namespace, and the
//! host reaches the port its endpoint declares at the workload's address, as it reaches a
//! container's. Needs what the runtime tests need, and nftables.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

mod support;

use korpis_helper::network::Network;
use korpis_proto::korpis::helper::v1 as helper;
use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};
use support::{Rig, live};

/// A workload's network, released however the test ends.
struct Held(Network, String);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.release(&self.1);
    }
}

/// What the first connection `listener` accepts within `within` sends, or what failed.
fn first_sent(listener: &TcpListener, within: Duration) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let until = Instant::now() + within;
    loop {
        match listener.accept() {
            Ok((mut s, _)) => {
                s.set_nonblocking(false).map_err(|e| e.to_string())?;
                s.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
                let mut text = String::new();
                let _ = s.read_to_string(&mut text);
                return Ok(text);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// What the guest at `at` sends to the first connection it accepts, tried until `within`.
fn answered(at: SocketAddr, within: Duration) -> Result<String, String> {
    let until = Instant::now() + within;
    loop {
        match TcpStream::connect_timeout(&at, Duration::from_secs(1)) {
            Ok(mut s) => {
                s.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
                let mut text = String::new();
                s.read_to_string(&mut text).map_err(|e| e.to_string())?;
                return Ok(text);
            }
            Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[tokio::test]
async fn a_guest_holds_its_workload_s_address_behind_the_tap_and_is_reached_at_it() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let network = Network::new(rig.machine.settings().netns_root.clone());
    let described = network.describe();
    if !described.offered {
        eprintln!("this machine offers no network ({}): not run", described.why_not);
        return;
    }
    let workload = format!("wl_net_{}", std::process::id());
    let ip = Ipv4Addr::new(10, 247, u8::try_from(std::process::id() % 250).unwrap(), 2);
    let script = "echo from-the-guest | nc 169.254.1.53 53; \
                  { ip -4 addr show eth0; cat /etc/resolv.conf; } > /tmp/said; \
                  while true; do nc -l -p 7001 < /tmp/said; done";
    let mut spec = rig.spec(&workload, script);
    let port = helper::Port {
        transport: helper::Transport::TRANSPORT_TCP.into(),
        port: 7001,
        ..Default::default()
    };
    let request = helper::EnsureNetworkRequest {
        workload: workload.clone(),
        ipv4: ip.to_string(),
        ipv4_pool: "10.247.0.0/16".into(),
        uid_base: spec.uid_base,
        uid_count: spec.uid_count,
        inbound: vec![port],
        tap: true,
        ..Default::default()
    };
    let made = network.ensure(&request).unwrap();
    let _held = Held(network.clone(), workload.clone());
    spec.namespaces = made.namespaces;
    spec.addresses = vec![format!("{ip}/32")];
    let resolver = network.open_resolver(&workload).unwrap();
    let resolver = TcpListener::from(resolver.into_iter().nth(1).unwrap());

    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    let asked = tokio::task::spawn_blocking(move || first_sent(&resolver, Duration::from_secs(60)));
    let asked = asked.await.unwrap().expect("the guest reaches its resolver's address");
    assert_eq!(asked.trim(), "from-the-guest");

    let at = SocketAddr::from((ip, 7001));
    let said = tokio::task::spawn_blocking(move || answered(at, Duration::from_secs(30)));
    let said = said.await.unwrap().expect("the host reaches the guest's declared port");
    assert!(said.contains(&format!("inet {ip}/32")), "the guest holds its address: {said}");
    assert!(said.contains("nameserver 169.254.1.53"), "and names its resolver: {said}");

    rig.machine.kill(&id).await.unwrap();
    rig.machine.destroy(&id).await.unwrap();
}
