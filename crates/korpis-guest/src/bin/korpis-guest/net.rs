// SPDX-License-Identifier: AGPL-3.0-or-later

//! The guest's network: loopback up, and `eth0`, the microVM's one interface, up with the
//! workload's addresses and a default route through each gateway, taken to be on the link, as the
//! helper sets the link of a container (§4.1 of docs/07-networking.md). Set over route netlink,
//! since the image need not hold any tool that would.

use korpis_guest::config::Network;
use netlink_packet_core::{
    NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage,
    NetlinkPayload,
};
use netlink_packet_route::address::{AddressAttribute, AddressHeaderFlags, AddressMessage};
use netlink_packet_route::link::{LinkAttribute, LinkFlags, LinkMessage};
use netlink_packet_route::route::{
    RouteAddress, RouteAttribute, RouteFlags, RouteMessage, RouteProtocol, RouteScope, RouteType,
};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_sys::protocols::NETLINK_ROUTE;
use netlink_sys::{Socket, SocketAddr};
use std::io;
use std::net::IpAddr;

/// The main routing table.
const MAIN_TABLE: u8 = 254;

struct Rtnl {
    socket: Socket,
    seq: u32,
}

fn family(ip: IpAddr) -> AddressFamily {
    if ip.is_ipv4() { AddressFamily::Inet } else { AddressFamily::Inet6 }
}

fn route_address(ip: IpAddr) -> RouteAddress {
    match ip {
        IpAddr::V4(v4) => RouteAddress::Inet(v4),
        IpAddr::V6(v6) => RouteAddress::Inet6(v6),
    }
}

/// The messages of `datagram` answering request `seq`, and whether one ended it.
fn answers(datagram: &[u8], seq: u32) -> io::Result<(Vec<RouteNetlinkMessage>, Option<i32>)> {
    let (mut found, mut at) = (Vec::new(), 0);
    while let Some(rest) = datagram.get(at..).filter(|r| !r.is_empty()) {
        let answer = NetlinkMessage::<RouteNetlinkMessage>::deserialize(rest)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        // Each message is padded to four bytes; a header is sixteen.
        at += (usize::try_from(answer.header.length).unwrap_or(0).max(16) + 3) & !3;
        if answer.header.sequence_number != seq {
            continue;
        }
        match answer.payload {
            NetlinkPayload::Error(e) => return Ok((found, Some(e.code.map_or(0, i32::from)))),
            NetlinkPayload::Done(_) => return Ok((found, Some(0))),
            NetlinkPayload::InnerMessage(m) => found.push(m),
            _ => {}
        }
    }
    Ok((found, None))
}

impl Rtnl {
    fn open() -> io::Result<Self> {
        let mut socket = Socket::new(NETLINK_ROUTE)?;
        socket.bind_auto()?;
        socket.connect(&SocketAddr::new(0, 0))?;
        Ok(Self { socket, seq: 0 })
    }

    /// Sends `message` and reads its answers; an error the kernel answers is returned, but for
    /// one that says what was asked for is already there.
    fn ask(
        &mut self,
        message: RouteNetlinkMessage,
        flags: u16,
    ) -> io::Result<Vec<RouteNetlinkMessage>> {
        self.seq = self.seq.wrapping_add(1);
        let mut header = NetlinkHeader::default();
        header.flags = flags | NLM_F_REQUEST | NLM_F_ACK;
        header.sequence_number = self.seq;
        let mut request = NetlinkMessage::new(header, NetlinkPayload::from(message));
        request.finalize();
        let mut bytes = vec![0; request.buffer_len()];
        request.serialize(&mut bytes);
        self.socket.send(&bytes, 0)?;
        let mut all = Vec::new();
        loop {
            let (datagram, _) = self.socket.recv_from_full()?;
            let (found, ended) = answers(&datagram, self.seq)?;
            all.extend(found);
            match ended {
                None => {}
                Some(0) => return Ok(all),
                Some(c) if -c == nix::libc::EEXIST => return Ok(all),
                Some(c) => return Err(io::Error::from_raw_os_error(-c)),
            }
        }
    }

    fn index(&mut self, name: &str) -> io::Result<u32> {
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(name.to_owned()));
        let found = self.ask(RouteNetlinkMessage::GetLink(message), 0)?;
        found
            .into_iter()
            .find_map(|m| {
                if let RouteNetlinkMessage::NewLink(l) = m { Some(l.header.index) } else { None }
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no link {name}")))
    }

    fn up(&mut self, index: u32) -> io::Result<()> {
        let mut message = LinkMessage::default();
        message.header.index = index;
        message.header.flags = LinkFlags::Up;
        message.header.change_mask = LinkFlags::Up;
        self.ask(RouteNetlinkMessage::SetLink(message), 0).map(drop)
    }

    fn address(&mut self, index: u32, (ip, prefix): (IpAddr, u8)) -> io::Result<()> {
        let mut message = AddressMessage::default();
        message.header.family = family(ip);
        message.header.prefix_len = prefix;
        message.header.index = index;
        // The address is the workload's alone: duplicate detection would only delay it.
        message.header.flags = AddressHeaderFlags::Nodad;
        message.attributes.push(AddressAttribute::Address(ip));
        if ip.is_ipv4() {
            message.attributes.push(AddressAttribute::Local(ip));
        }
        let create = NLM_F_CREATE | NLM_F_EXCL;
        self.ask(RouteNetlinkMessage::NewAddress(message), create).map(drop)
    }

    fn default_route(&mut self, index: u32, gateway: IpAddr) -> io::Result<()> {
        let mut message = RouteMessage::default();
        message.header.address_family = family(gateway);
        message.header.table = MAIN_TABLE;
        message.header.protocol = RouteProtocol::Static;
        message.header.kind = RouteType::Unicast;
        message.header.scope = RouteScope::Universe;
        message.header.flags = RouteFlags::Onlink;
        message.attributes.push(RouteAttribute::Oif(index));
        message.attributes.push(RouteAttribute::Gateway(route_address(gateway)));
        let create = NLM_F_CREATE | NLM_F_EXCL;
        self.ask(RouteNetlinkMessage::NewRoute(message), create).map(drop)
    }
}

fn set(network: Option<&Network>) -> io::Result<()> {
    let mut nl = Rtnl::open()?;
    let lo = nl.index("lo")?;
    nl.up(lo)?;
    let Some(network) = network else { return Ok(()) };
    let eth = nl.index("eth0")?;
    nl.up(eth)?;
    for a in &network.addresses {
        nl.address(eth, *a)?;
    }
    for g in &network.gateways {
        nl.default_route(eth, *g)?;
    }
    Ok(())
}

/// Sets the guest's network as `network` says, or loopback alone.
pub fn configure(network: Option<&Network>) -> Result<(), String> {
    set(network).map_err(|e| format!("setting the network: {e}"))
}
