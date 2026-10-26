// SPDX-License-Identifier: AGPL-3.0-or-later

//! A host address on one of the node's links, for a dedicated address answered on it (§3.7 of
//! docs/07-networking.md): added without a route of its prefix, and removed.

use super::{Answer, Rtnl, ip_family};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::address::{AddressAttribute, AddressMessage};
use std::io;
use std::net::{IpAddr, Ipv4Addr};

fn host_address(index: u32, address: Ipv4Addr) -> AddressMessage {
    let ip = IpAddr::V4(address);
    let mut message = AddressMessage::default();
    message.header.family = ip_family(ip);
    message.header.prefix_len = 32;
    message.header.index = index;
    message.attributes.push(AddressAttribute::Address(ip));
    message.attributes.push(AddressAttribute::Local(ip));
    message
}

impl Rtnl {
    /// Adds `address/32` to link `index`; whether this added it, rather than finding it there.
    pub(crate) fn add_host_address(&mut self, index: u32, address: Ipv4Addr) -> io::Result<bool> {
        let message = RouteNetlinkMessage::NewAddress(host_address(index, address));
        Ok(matches!(self.request(message, true)?, Answer::Done))
    }

    /// Removes `address/32` from link `index`; whether it was there.
    pub(crate) fn delete_host_address(
        &mut self,
        index: u32,
        address: Ipv4Addr,
    ) -> io::Result<bool> {
        let message = RouteNetlinkMessage::DelAddress(host_address(index, address));
        Ok(matches!(self.request(message, false)?, Answer::Done))
    }
}
