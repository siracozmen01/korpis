// SPDX-License-Identifier: AGPL-3.0-or-later

//! The GRE links between an edge and the nodes it forwards to (§3.1 of docs/07-networking.md):
//! one per pair, from this node's mesh address to the other's, made, listed by their names'
//! beginning, and removed.

use super::{Answer, Rtnl};
use netlink_packet_core::{NLM_F_DUMP, NLM_F_REQUEST, NetlinkPayload};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{
    InfoData, InfoGre, InfoKind, LinkAttribute, LinkInfo, LinkMessage,
};
use std::io;
use std::net::Ipv4Addr;

/// The name of a link's message, if it has one.
fn name_of(m: &LinkMessage) -> Option<String> {
    m.attributes.iter().find_map(|a| match a {
        LinkAttribute::IfName(n) => Some(n.clone()),
        _ => None,
    })
}

impl Rtnl {
    /// Makes a GRE link named `name` from `local` to `remote`; whether it was made, a link
    /// already there being left as it is.
    pub(crate) fn add_gre(
        &mut self,
        name: &str,
        (local, remote): (Ipv4Addr, Ipv4Addr),
    ) -> io::Result<bool> {
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(name.to_owned()));
        message.attributes.push(LinkAttribute::LinkInfo(vec![
            LinkInfo::Kind(InfoKind::GreTun),
            LinkInfo::Data(InfoData::GreTun(vec![
                InfoGre::Local(local),
                InfoGre::Remote(remote),
                InfoGre::Ttl(64),
                InfoGre::PathMTUDiscovery(true),
            ])),
        ]));
        Ok(matches!(self.request(RouteNetlinkMessage::NewLink(message), true)?, Answer::Done))
    }

    /// Every link whose name begins with `prefix`, with its index.
    pub(crate) fn links_named(&mut self, prefix: &str) -> io::Result<Vec<(u32, String)>> {
        let ask = RouteNetlinkMessage::GetLink(LinkMessage::default());
        let mut found = Vec::new();
        for answer in self.exchange(ask, NLM_F_REQUEST | NLM_F_DUMP)? {
            match answer.payload {
                NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(m)) => {
                    let named = name_of(&m).filter(|n| n.starts_with(prefix));
                    found.extend(named.map(|name| (m.header.index, name)));
                }
                NetlinkPayload::Error(e) if e.code.is_some() => return Err(e.to_io()),
                _ => {}
            }
        }
        Ok(found)
    }
}
