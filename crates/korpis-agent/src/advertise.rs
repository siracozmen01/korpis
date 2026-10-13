// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node advertises for inbound traffic (§9 of docs/07-networking.md): the addresses its
//! operator names, or, when they name none, the address the node's traffic towards the control
//! plane leaves from, if that address is routable on the internet. A node behind NAT therefore
//! advertises nothing of its own accord, which is the truth: nobody outside can reach it at an
//! address it can see. Nothing here is a claim of reachability; the control plane measures that.

use korpis_core::node::advertised::{PortRange, is_global};
use korpis_proto::korpis::agent::v1 as agent;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _, UdpSocket};

/// What to advertise, and how to find it.
#[derive(Debug, Clone)]
pub struct Advertise {
    declared: Vec<IpAddr>,
    ports: PortRange,
    /// The control plane's addresses, resolved once at start.
    towards: Vec<SocketAddr>,
}

/// The local address the kernel would send from towards `to`. Connecting a UDP socket sends
/// nothing; it only chooses the route.
fn source(to: SocketAddr) -> Option<IpAddr> {
    let any: SocketAddr =
        if to.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
    let socket = UdpSocket::bind(any).ok()?;
    socket.connect(to).ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

/// The host and port of `url` (`https://host:port`), resolved; empty if it does not resolve.
fn resolve(url: &str) -> Vec<SocketAddr> {
    let Ok(uri) = url.parse::<http::Uri>() else { return Vec::new() };
    let Some(host) = uri.host() else { return Vec::new() };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(443);
    (host, port).to_socket_addrs().map(Iterator::collect).unwrap_or_default()
}

impl Advertise {
    /// The operator's `declared` addresses and `ports`; with none declared, the node's own
    /// routable address towards the control plane at `server`, which is resolved now.
    #[must_use]
    pub fn new(declared: Vec<IpAddr>, ports: PortRange, server: &str) -> Self {
        let towards = if declared.is_empty() { resolve(server) } else { Vec::new() };
        Self { declared, ports, towards }
    }

    /// What to advertise now. Detection is repeated each time, so an address that changes is
    /// reported at the next heartbeat.
    #[must_use]
    pub fn current(&self) -> agent::Advertised {
        let mut addresses = self.declared.clone();
        if addresses.is_empty() {
            addresses =
                self.towards.iter().filter_map(|t| source(*t)).filter(|a| is_global(*a)).collect();
            addresses.sort_unstable();
            addresses.dedup();
        }
        agent::Advertised {
            addresses: addresses.iter().map(ToString::to_string).collect(),
            first_port: u32::from(self.ports.first()),
            last_port: u32::from(self.ports.last()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_addresses_are_advertised_as_they_are() {
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let a = Advertise::new(vec![lan], PortRange::DEFAULT, "https://localhost:7443");
        let now = a.current();
        assert_eq!(now.addresses, ["192.168.1.20"], "the operator's word, even when private");
        assert_eq!((now.first_port, now.last_port), (30_000, 32_767));
    }

    #[test]
    fn a_detected_address_is_advertised_only_when_it_is_routable() {
        let a = Advertise::new(Vec::new(), PortRange::DEFAULT, "https://127.0.0.1:7443");
        assert!(a.current().addresses.is_empty(), "loopback is no address for clients");
    }
}
