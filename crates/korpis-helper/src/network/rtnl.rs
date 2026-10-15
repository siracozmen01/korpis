// SPDX-License-Identifier: AGPL-3.0-or-later

//! The few route-netlink requests a workload's network needs (§4.1 of docs/07-networking.md):
//! making a veth pair whose peer is in another namespace, finding and removing a link, bringing
//! one up, and adding an address or a route. A socket belongs to the namespace it was opened in,
//! so one opened on a thread inside a workload's namespace configures that namespace.

use netlink_packet_core::{
    NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage,
    NetlinkPayload,
};
use netlink_packet_route::address::{AddressAttribute, AddressHeaderFlags, AddressMessage};
use netlink_packet_route::link::{
    InfoData, InfoKind, InfoVeth, LinkAttribute, LinkFlags, LinkInfo, LinkMessage,
};
use netlink_packet_route::route::{
    RouteAddress, RouteAttribute, RouteFlags, RouteMessage, RouteProtocol, RouteScope, RouteType,
};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_sys::protocols::NETLINK_ROUTE;
use netlink_sys::{Socket, SocketAddr};
use std::io;
use std::net::IpAddr;
use std::os::fd::RawFd;

/// The main routing table.
const MAIN_TABLE: u8 = 254;

/// A route-netlink socket in the namespace it was opened in.
pub(crate) struct Rtnl {
    socket: Socket,
    seq: u32,
}

/// What the kernel answered a request that asked for no data.
enum Answer {
    Done,
    Exists,
    Missing,
}

fn ip_family(ip: IpAddr) -> AddressFamily {
    if ip.is_ipv4() { AddressFamily::Inet } else { AddressFamily::Inet6 }
}

fn route_address(ip: IpAddr) -> RouteAddress {
    match ip {
        IpAddr::V4(v4) => RouteAddress::Inet(v4),
        IpAddr::V6(v6) => RouteAddress::Inet6(v6),
    }
}

/// Adds to `answers` the messages of `datagram` answering request `seq`; whether one ended it.
fn read_datagram(
    datagram: &[u8],
    seq: u32,
    answers: &mut Vec<NetlinkMessage<RouteNetlinkMessage>>,
) -> io::Result<bool> {
    let mut at = 0;
    while let Some(rest) = datagram.get(at..).filter(|r| !r.is_empty()) {
        let answer = NetlinkMessage::<RouteNetlinkMessage>::deserialize(rest)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        // Each message is padded to four bytes; a header is sixteen.
        at += (usize::try_from(answer.header.length).unwrap_or(0).max(16) + 3) & !3;
        if answer.header.sequence_number != seq {
            continue;
        }
        let last = matches!(answer.payload, NetlinkPayload::Error(_) | NetlinkPayload::Done(_));
        answers.push(answer);
        if last {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Rtnl {
    /// A socket in the calling thread's network namespace.
    pub(crate) fn open() -> io::Result<Self> {
        let mut socket = Socket::new(NETLINK_ROUTE)?;
        socket.bind_auto()?;
        socket.connect(&SocketAddr::new(0, 0))?;
        Ok(Self { socket, seq: 0 })
    }

    /// Sends `message` with `flags` and reads every answer to it.
    fn exchange(
        &mut self,
        message: RouteNetlinkMessage,
        flags: u16,
    ) -> io::Result<Vec<NetlinkMessage<RouteNetlinkMessage>>> {
        self.seq = self.seq.wrapping_add(1);
        let mut header = NetlinkHeader::default();
        header.flags = flags;
        header.sequence_number = self.seq;
        let mut request = NetlinkMessage::new(header, NetlinkPayload::from(message));
        request.finalize();
        let mut bytes = vec![0; request.buffer_len()];
        request.serialize(&mut bytes);
        self.socket.send(&bytes, 0)?;
        let mut answers = Vec::new();
        loop {
            let (datagram, _) = self.socket.recv_from_full()?;
            if read_datagram(&datagram, self.seq, &mut answers)? {
                return Ok(answers);
            }
        }
    }

    /// Sends a request that asks for no data and reads whether it held.
    fn request(&mut self, message: RouteNetlinkMessage, create: bool) -> io::Result<Answer> {
        let flags = NLM_F_REQUEST | NLM_F_ACK | if create { NLM_F_CREATE | NLM_F_EXCL } else { 0 };
        for answer in self.exchange(message, flags)? {
            if let NetlinkPayload::Error(e) = answer.payload {
                return match e.code.map(i32::from) {
                    None => Ok(Answer::Done),
                    Some(c) if -c == libc::EEXIST => Ok(Answer::Exists),
                    Some(c) if -c == libc::ENODEV || -c == libc::ENOENT => Ok(Answer::Missing),
                    Some(_) => Err(e.to_io()),
                };
            }
        }
        Ok(Answer::Done)
    }

    /// The index of the link named `name`, if there is one.
    pub(crate) fn link_index(&mut self, name: &str) -> io::Result<Option<u32>> {
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(name.to_owned()));
        // Asked with an acknowledgement, so the answer ends with one: a link named is not a dump.
        let answers =
            self.exchange(RouteNetlinkMessage::GetLink(message), NLM_F_REQUEST | NLM_F_ACK)?;
        for answer in answers {
            match answer.payload {
                NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(l)) => {
                    return Ok(Some(l.header.index));
                }
                NetlinkPayload::Error(e) if e.code.is_some_and(|c| -c.get() == libc::ENODEV) => {
                    return Ok(None);
                }
                NetlinkPayload::Error(e) if e.code.is_some() => return Err(e.to_io()),
                _ => {}
            }
        }
        Ok(None)
    }

    /// Makes a veth pair: `host` here, and `peer` moved into the namespace open as `namespace`.
    /// Whether it was made; a pair already there is left as it is.
    pub(crate) fn add_veth(
        &mut self,
        host: &str,
        peer: &str,
        namespace: RawFd,
    ) -> io::Result<bool> {
        let mut inner = LinkMessage::default();
        inner.attributes.push(LinkAttribute::IfName(peer.to_owned()));
        inner.attributes.push(LinkAttribute::NetNsFd(namespace));
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(host.to_owned()));
        message.attributes.push(LinkAttribute::LinkInfo(vec![
            LinkInfo::Kind(InfoKind::Veth),
            LinkInfo::Data(InfoData::Veth(InfoVeth::Peer(inner))),
        ]));
        Ok(matches!(self.request(RouteNetlinkMessage::NewLink(message), true)?, Answer::Done))
    }

    /// Brings link `index` up.
    pub(crate) fn set_up(&mut self, index: u32) -> io::Result<()> {
        let mut message = LinkMessage::default();
        message.header.index = index;
        message.header.flags = LinkFlags::Up;
        message.header.change_mask = LinkFlags::Up;
        self.request(RouteNetlinkMessage::SetLink(message), false).map(|_| ())
    }

    /// Removes link `index`, and with a veth its peer; whether it was there.
    pub(crate) fn delete_link(&mut self, index: u32) -> io::Result<bool> {
        let mut message = LinkMessage::default();
        message.header.index = index;
        Ok(matches!(self.request(RouteNetlinkMessage::DelLink(message), false)?, Answer::Done))
    }

    /// Adds `ip/prefix` to link `index`, unless it has it.
    pub(crate) fn add_address(&mut self, index: u32, ip: IpAddr, prefix: u8) -> io::Result<()> {
        let mut message = AddressMessage::default();
        message.header.family = ip_family(ip);
        message.header.prefix_len = prefix;
        message.header.index = index;
        // An address only this link holds needs no duplicate detection, which would leave an
        // IPv6 address unusable for its first second.
        message.header.flags = AddressHeaderFlags::Nodad;
        message.attributes.push(AddressAttribute::Address(ip));
        if ip.is_ipv4() {
            message.attributes.push(AddressAttribute::Local(ip));
        }
        self.request(RouteNetlinkMessage::NewAddress(message), true).map(|_| ())
    }

    /// Routes `dest/prefix` out of link `index`, through `gateway` when one is named, which is
    /// taken to be on the link; unless the route is there.
    pub(crate) fn add_route(
        &mut self,
        (dest, prefix): (IpAddr, u8),
        index: u32,
        gateway: Option<IpAddr>,
    ) -> io::Result<()> {
        let mut message = RouteMessage::default();
        message.header.address_family = ip_family(dest);
        message.header.destination_prefix_length = prefix;
        message.header.table = MAIN_TABLE;
        message.header.protocol = RouteProtocol::Static;
        message.header.kind = RouteType::Unicast;
        message.header.scope =
            if gateway.is_some() { RouteScope::Universe } else { RouteScope::Link };
        if prefix > 0 {
            message.attributes.push(RouteAttribute::Destination(route_address(dest)));
        }
        message.attributes.push(RouteAttribute::Oif(index));
        if let Some(g) = gateway {
            message.header.flags = RouteFlags::Onlink;
            message.attributes.push(RouteAttribute::Gateway(route_address(g)));
        }
        self.request(RouteNetlinkMessage::NewRoute(message), true).map(|_| ())
    }
}
