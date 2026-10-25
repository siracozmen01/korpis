// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's side of the mesh (§9.1 of docs/07-networking.md): one WireGuard interface, made
//! with a key the helper generates the first time and keeps in its state, whose private half is
//! never answered to anyone; the peers the agent names, each allowed its workload pools, every
//! other removed; and a route to each of those pools through the interface, every other route
//! through it removed. WireGuard accepts a packet from a peer only with a source in that peer's
//! allowed IPs, so what arrives on the interface comes from the node holding its source's pool;
//! of that, only the dependents the agent names reach a workload, each its own endpoint.

mod admit;
#[cfg(test)]
mod tests;
mod wg;

use crate::error::{HelperError, HelperResult};
use crate::network::rtnl::Rtnl;
use ipnet::IpNet;
use korpis_proto::korpis::helper::v1 as helper;
use netlink_packet_wireguard::{
    WireguardAddressFamily, WireguardAllowedIp, WireguardAllowedIpAttr, WireguardAttribute,
    WireguardPeer, WireguardPeerAttribute, WireguardPeerFlags,
};
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
pub(crate) use wg::Key;

/// The interface's name.
pub const INTERFACE: &str = "korpis-overlay";
/// Its MTU: a 1,500-byte link less WireGuard's 80 bytes over IPv6 (§5 of
/// docs/research/evidence.md).
pub const MTU: u32 = 1420;
/// How often a peer is sent a keepalive, which keeps a NAT's mapping open for a peer behind it.
pub const KEEPALIVE_SECONDS: u16 = 25;

/// One peer, as the request names it and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub key: Key,
    pub endpoint: Option<SocketAddr>,
    pub allowed: Vec<IpNet>,
}

/// The peers `r` names, checked: a key of 32 bytes, an endpoint that is an address and a port,
/// and allowed IPs that are networks.
///
/// # Errors
/// The first that is not, and why.
pub(crate) fn peers(r: &helper::KeepOverlayRequest) -> HelperResult<Vec<Peer>> {
    r.peers
        .iter()
        .map(|p| {
            let key: Key =
                p.public_key.as_slice().try_into().map_err(|_| {
                    HelperError::Refused("a peer's public key is not 32 bytes".into())
                })?;
            let endpoint = if p.endpoint.is_empty() {
                None
            } else {
                Some(p.endpoint.parse().map_err(|_| {
                    HelperError::Refused(format!("{:?} is no address and port", p.endpoint))
                })?)
            };
            let allowed = p.allowed_ips.iter().map(|a| {
                let net: IpNet =
                    a.parse().map_err(|_| HelperError::Refused(format!("{a:?} is no network")))?;
                Ok(net.trunc())
            });
            Ok(Peer { key, endpoint, allowed: allowed.collect::<HelperResult<_>>()? })
        })
        .collect()
}

/// `net` as WireGuard's allowed IP.
fn allowed_ip(net: IpNet) -> WireguardAllowedIp {
    let family = match net {
        IpNet::V4(_) => WireguardAddressFamily::Ipv4,
        IpNet::V6(_) => WireguardAddressFamily::Ipv6,
    };
    WireguardAllowedIp(vec![
        WireguardAllowedIpAttr::Family(family),
        WireguardAllowedIpAttr::IpAddr(net.addr()),
        WireguardAllowedIpAttr::Cidr(net.prefix_len()),
    ])
}

/// The device's configuration: its private key when it has none yet, its port, each of `peers`
/// made or updated in place, its allowed IPs replaced, and each of `stale` removed. Peers are
/// never replaced wholesale, which would end every session and its handshake on each call.
pub(crate) fn device(
    private: Option<Key>,
    port: u16,
    (peers, stale): (&[Peer], &[Key]),
) -> Vec<WireguardAttribute> {
    let kept = peers.iter().map(|p| {
        let mut attrs = vec![
            WireguardPeerAttribute::PublicKey(p.key),
            WireguardPeerAttribute::Flags(WireguardPeerFlags::ReplaceAllowedIps),
            WireguardPeerAttribute::PersistentKeepalive(KEEPALIVE_SECONDS),
            WireguardPeerAttribute::AllowedIps(p.allowed.iter().copied().map(allowed_ip).collect()),
        ];
        attrs.extend(p.endpoint.map(WireguardPeerAttribute::Endpoint));
        WireguardPeer(attrs)
    });
    let removed = stale.iter().map(|k| {
        WireguardPeer(vec![
            WireguardPeerAttribute::PublicKey(*k),
            WireguardPeerAttribute::Flags(WireguardPeerFlags::RemoveMe),
        ])
    });
    let mut attrs = vec![WireguardAttribute::IfName(INTERFACE.to_owned())];
    attrs.extend(private.map(WireguardAttribute::PrivateKey));
    attrs.push(WireguardAttribute::ListenPort(port));
    attrs.push(WireguardAttribute::Peers(kept.chain(removed).collect()));
    attrs
}

/// The node's mesh interface, its private key kept at `key`.
#[derive(Debug, Clone)]
pub struct Overlay {
    key: PathBuf,
}

impl Overlay {
    /// The interface whose private key is kept at `key`, made the first time it is asked for.
    #[must_use]
    pub fn new(key: PathBuf) -> Self {
        Self { key }
    }

    /// The private key, made the first time: 32 random bytes clamped as Curve25519 requires,
    /// written readable by root alone.
    fn private(&self) -> HelperResult<Key> {
        let failed = |e: std::io::Error| HelperError::Failed(format!("the overlay key: {e}"));
        let mut key: Key = [0; 32];
        if let Ok(mut file) = std::fs::File::open(&self.key) {
            file.read_exact(&mut key).map_err(failed)?;
            return Ok(key);
        }
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut key))
            .map_err(failed)?;
        key[0] &= 0b1111_1000;
        key[31] &= 0b0111_1111;
        key[31] |= 0b0100_0000;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.key)
            .map_err(failed)?;
        file.write_all(&key).and_then(|()| file.sync_all()).map_err(failed)?;
        Ok(key)
    }

    /// Keeps the interface to `r`: made and up, its peers those `r` names, a route to each one's
    /// allowed IPs through it and no other, and what it admits what `r` names; its public key and
    /// the peers' handshakes.
    ///
    /// # Errors
    /// A peer `r` names wrongly, a kernel without WireGuard, or netlink refusing a change.
    pub fn keep(
        &self,
        r: &helper::KeepOverlayRequest,
    ) -> HelperResult<helper::KeepOverlayResponse> {
        let (peers, admitted) = (peers(r)?, admit::admitted(r)?);
        let port = u16::try_from(r.port)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| HelperError::Refused(format!("{} is no UDP port", r.port)))?;
        let failed = |e: std::io::Error| HelperError::Failed(format!("the overlay: {e}"));
        let mut rtnl = Rtnl::open().map_err(failed)?;
        rtnl.add_wireguard(INTERFACE).map_err(failed)?;
        let index = rtnl
            .link_index(INTERFACE)
            .map_err(failed)?
            .ok_or_else(|| HelperError::Failed("the overlay's link is gone".into()))?;
        rtnl.set_mtu(index, MTU).map_err(failed)?;
        let mut wg = wg::Wg::open().map_err(failed)?;
        let before = wg.get(INTERFACE).map_err(failed)?;
        // A link made just now has no key; one kept since keeps its own.
        let private = if before.public_key.is_none() { Some(self.private()?) } else { None };
        let stale: Vec<Key> = before
            .handshakes
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| !peers.iter().any(|p| p.key == *k))
            .collect();
        wg.set(device(private, port, (&peers, &stale))).map_err(failed)?;
        rtnl.set_up(index).map_err(failed)?;
        routes(&mut rtnl, index, &peers).map_err(failed)?;
        crate::network::nft::apply(&admit::rules(&admitted)).map_err(failed)?;
        let device = wg.get(INTERFACE).map_err(failed)?;
        Ok(answer(&device))
    }
}

/// Keeps the routes out of link `index` to the allowed IPs of `peers`, and no other.
fn routes(rtnl: &mut Rtnl, index: u32, peers: &[Peer]) -> std::io::Result<()> {
    let wanted: Vec<(IpAddr, u8)> =
        peers.iter().flat_map(|p| p.allowed.iter().map(|n| (n.addr(), n.prefix_len()))).collect();
    for stale in rtnl.routes_of(index)?.into_iter().filter(|r| !wanted.contains(r)) {
        rtnl.delete_route(stale, index)?;
    }
    for route in wanted {
        rtnl.add_route(route, index, None)?;
    }
    Ok(())
}

/// What the helper answers of `device`.
fn answer(device: &wg::Device) -> helper::KeepOverlayResponse {
    let handshakes = device.handshakes.iter().map(|(key, at)| helper::PeerHandshake {
        public_key: key.to_vec(),
        latest_unix_seconds: at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0),
        ..Default::default()
    });
    helper::KeepOverlayResponse {
        public_key: device.public_key.map(|k| k.to_vec()).unwrap_or_default(),
        handshakes: handshakes.collect(),
        ..Default::default()
    }
}
