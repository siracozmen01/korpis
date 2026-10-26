// SPDX-License-Identifier: AGPL-3.0-or-later

//! A host address on one of the node's links, for a dedicated address answered on it (§3.7 of
//! docs/07-networking.md) and an edge's held address (§3.1 there): added without a route of its
//! prefix, an IPv6 one without duplicate detection, since only this link holds it and detection
//! would leave it unusable for its first second; and removed.

use super::{Answer, Rtnl, ip_family};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::address::{AddressAttribute, AddressHeaderFlags, AddressMessage};
use std::io;
use std::net::IpAddr;

fn host_address(index: u32, ip: IpAddr) -> AddressMessage {
    let mut message = AddressMessage::default();
    message.header.family = ip_family(ip);
    message.header.prefix_len = if ip.is_ipv4() { 32 } else { 128 };
    message.header.index = index;
    message.attributes.push(AddressAttribute::Address(ip));
    if ip.is_ipv4() {
        message.attributes.push(AddressAttribute::Local(ip));
    } else {
        message.header.flags = AddressHeaderFlags::Nodad;
    }
    message
}

impl Rtnl {
    /// Adds `address` as a host address to link `index`; whether this added it, rather than
    /// finding it there.
    pub(crate) fn add_host_address(
        &mut self,
        index: u32,
        address: impl Into<IpAddr>,
    ) -> io::Result<bool> {
        let message = RouteNetlinkMessage::NewAddress(host_address(index, address.into()));
        Ok(matches!(self.request(message, true)?, Answer::Done))
    }

    /// Removes `address` from link `index`; whether it was there.
    pub(crate) fn delete_host_address(
        &mut self,
        index: u32,
        address: impl Into<IpAddr>,
    ) -> io::Result<bool> {
        let message = RouteNetlinkMessage::DelAddress(host_address(index, address.into()));
        Ok(matches!(self.request(message, false)?, Answer::Done))
    }
}
