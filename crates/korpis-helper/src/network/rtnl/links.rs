// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the mesh's interface needs of route netlink (§9.1 of docs/07-networking.md): a WireGuard
//! link, its MTU, and the routes out of it, read and removed as well as added.

use super::{Answer, MAIN_TABLE, Rtnl, ip_family, route_address};
use netlink_packet_core::{NLM_F_DUMP, NLM_F_REQUEST, NetlinkPayload};
use netlink_packet_route::link::{InfoKind, LinkAttribute, LinkInfo, LinkMessage};
use netlink_packet_route::route::{RouteAddress, RouteAttribute, RouteMessage, RouteScope};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use std::io;
use std::net::IpAddr;

/// A route's destination as an address.
pub(super) fn destination(a: &RouteAddress) -> Option<IpAddr> {
    match a {
        RouteAddress::Inet(v4) => Some(IpAddr::V4(*v4)),
        RouteAddress::Inet6(v6) => Some(IpAddr::V6(*v6)),
        _ => None,
    }
}

/// `route`'s destination and prefix, if it is the main table's and out of link `index`.
fn out_of(route: &RouteMessage, index: u32) -> Option<(IpAddr, u8)> {
    if !route.attributes.contains(&RouteAttribute::Oif(index)) || route.header.table != MAIN_TABLE {
        return None;
    }
    let dest = route.attributes.iter().find_map(|a| match a {
        RouteAttribute::Destination(d) => destination(d),
        _ => None,
    })?;
    Some((dest, route.header.destination_prefix_length))
}

/// What one answer to a dump of routes says of the routes out of link `index`.
fn read_route(
    payload: NetlinkPayload<RouteNetlinkMessage>,
    index: u32,
) -> io::Result<Option<(IpAddr, u8)>> {
    match payload {
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(r)) => Ok(out_of(&r, index)),
        NetlinkPayload::Error(e) if e.code.is_some() => Err(e.to_io()),
        _ => Ok(None),
    }
}

impl Rtnl {
    /// Makes a WireGuard link named `name`; whether it was made, a link already there being
    /// left as it is.
    pub(crate) fn add_wireguard(&mut self, name: &str) -> io::Result<bool> {
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(name.to_owned()));
        message.attributes.push(LinkAttribute::LinkInfo(vec![LinkInfo::Kind(InfoKind::Wireguard)]));
        Ok(matches!(self.request(RouteNetlinkMessage::NewLink(message), true)?, Answer::Done))
    }

    /// Sets link `index`'s MTU.
    pub(crate) fn set_mtu(&mut self, index: u32, mtu: u32) -> io::Result<()> {
        let mut message = LinkMessage::default();
        message.header.index = index;
        message.attributes.push(LinkAttribute::Mtu(mtu));
        self.request(RouteNetlinkMessage::SetLink(message), false).map(|_| ())
    }

    /// The destinations of the main table's routes out of link `index`, each with its prefix.
    pub(crate) fn routes_of(&mut self, index: u32) -> io::Result<Vec<(IpAddr, u8)>> {
        let mut found = Vec::new();
        for family in [AddressFamily::Inet, AddressFamily::Inet6] {
            let mut message = RouteMessage::default();
            message.header.address_family = family;
            let ask = RouteNetlinkMessage::GetRoute(message);
            let answers = self.exchange(ask, NLM_F_REQUEST | NLM_F_DUMP)?;
            for payload in answers.into_iter().map(|a| a.payload) {
                found.extend(read_route(payload, index)?);
            }
        }
        Ok(found)
    }

    /// Removes the main table's route to `dest/prefix` out of link `index`, if it is there.
    pub(crate) fn delete_route(
        &mut self,
        (dest, prefix): (IpAddr, u8),
        index: u32,
    ) -> io::Result<()> {
        let mut message = RouteMessage::default();
        message.header.address_family = ip_family(dest);
        message.header.destination_prefix_length = prefix;
        message.header.table = MAIN_TABLE;
        message.header.scope = RouteScope::Link;
        message.attributes.push(RouteAttribute::Destination(route_address(dest)));
        message.attributes.push(RouteAttribute::Oif(index));
        self.request(RouteNetlinkMessage::DelRoute(message), false).map(|_| ())
    }
}
