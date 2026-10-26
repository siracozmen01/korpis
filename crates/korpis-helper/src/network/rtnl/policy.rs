// SPDX-License-Identifier: AGPL-3.0-or-later

//! The policy routing edges need (§3.1 of docs/07-networking.md): a rule sending packets of one
//! mark to a table of Korpis's own, and that table's routes, each out of a GRE link; and for
//! each such link, rules routing what arrives through it by the main table alone. Every rule is
//! made for IPv4 and IPv6 alike, since one link carries both, and a table holds the routes of
//! both. All are listed, so a call removes what it no longer asks for and leaves the rest in
//! place, and no packet of a flow is ever routed by the main table while its own is being
//! rewritten.

use super::{Answer, Rtnl, ip_family, route_address};
use netlink_packet_core::{NLM_F_CREATE, NLM_F_DUMP, NLM_F_REPLACE, NLM_F_REQUEST, NetlinkPayload};
use netlink_packet_route::route::{
    RouteAttribute, RouteHeader, RouteMessage, RouteProtocol, RouteScope, RouteType,
};
use netlink_packet_route::rule::{RuleAction, RuleAttribute, RuleMessage};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use std::io;
use std::net::IpAddr;

/// Where Korpis's rules sit: after the local table's, before the main table's.
pub(crate) const PRIORITY: u32 = 1000;

/// Where a GRE link's own rules sit, ahead of the marks': the first routes what arrives through
/// the link by the main table, and the second refuses what that table has no route for, so a
/// packet arriving with its link's mark is never routed by the mark back into the link. The mark
/// is still there for the kernel's check of its source, which so finds the link it came by.
pub(crate) const LINK_PRIORITY: u32 = PRIORITY - 2;

/// Where the rules of the node's own addresses in the mesh sit: after the main table's, so they
/// route only what the main table has no route for.
pub(crate) const FALLBACK_PRIORITY: u32 = 32_800;

/// One rule: packets whose mark, masked by `mask`, is `mark` look up `table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkRule {
    pub(crate) mark: u32,
    pub(crate) mask: u32,
    pub(crate) table: u32,
}

/// The families every rule is made in.
const FAMILIES: [AddressFamily; 2] = [AddressFamily::Inet, AddressFamily::Inet6];

fn rule_message(r: MarkRule, family: AddressFamily) -> RuleMessage {
    let mut m = RuleMessage::default();
    m.header.family = family;
    m.header.action = RuleAction::ToTable;
    m.attributes.push(RuleAttribute::Priority(PRIORITY));
    m.attributes.push(RuleAttribute::FwMark(r.mark));
    m.attributes.push(RuleAttribute::FwMask(r.mask));
    m.attributes.push(RuleAttribute::Table(r.table));
    m
}

/// Link `name`'s rules in `family`, the main table's lookup and then the refusal.
fn link_messages(name: &str, family: AddressFamily) -> [RuleMessage; 2] {
    let rule = |priority: u32, action: RuleAction| {
        let mut m = RuleMessage::default();
        m.header.family = family;
        m.header.action = action;
        m.attributes.push(RuleAttribute::Priority(priority));
        m.attributes.push(RuleAttribute::Iifname(name.to_owned()));
        m
    };
    let mut main = rule(LINK_PRIORITY, RuleAction::ToTable);
    main.header.table = RouteHeader::RT_TABLE_MAIN;
    main.attributes.push(RuleAttribute::Table(u32::from(RouteHeader::RT_TABLE_MAIN)));
    [main, rule(LINK_PRIORITY + 1, RuleAction::Unreachable)]
}

/// The rule sending what `from` sends, where the main table has no route for it, to `table`.
fn source_message(from: IpAddr, table: u32) -> RuleMessage {
    let mut m = RuleMessage::default();
    m.header.family = ip_family(from);
    m.header.src_len = if from.is_ipv4() { 32 } else { 128 };
    m.header.action = RuleAction::ToTable;
    m.attributes.push(RuleAttribute::Priority(FALLBACK_PRIORITY));
    m.attributes.push(RuleAttribute::Source(from));
    m.attributes.push(RuleAttribute::Table(table));
    m
}

/// The source of a rule at the fallback priority.
fn read_source_rule(m: &RuleMessage) -> Option<IpAddr> {
    let at = m.attributes.iter().find_map(|a| match a {
        RuleAttribute::Priority(p) => Some(*p),
        _ => None,
    });
    if at != Some(FALLBACK_PRIORITY) {
        return None;
    }
    m.attributes.iter().find_map(|a| match a {
        RuleAttribute::Source(from) => Some(*from),
        _ => None,
    })
}

/// The link a rule at a link's priority is for.
fn read_link_rule(m: &RuleMessage) -> Option<String> {
    let at = m.attributes.iter().find_map(|a| match a {
        RuleAttribute::Priority(p) => Some(*p),
        _ => None,
    });
    if !matches!(at, Some(p) if p == LINK_PRIORITY || p == LINK_PRIORITY + 1) {
        return None;
    }
    m.attributes.iter().find_map(|a| match a {
        RuleAttribute::Iifname(n) => Some(n.clone()),
        _ => None,
    })
}

fn read_rule(m: &RuleMessage) -> Option<MarkRule> {
    let mut found = (None, None, None, None);
    for a in &m.attributes {
        match a {
            RuleAttribute::FwMark(v) => found.0 = Some(*v),
            RuleAttribute::FwMask(v) => found.1 = Some(*v),
            RuleAttribute::Table(v) => found.2 = Some(*v),
            RuleAttribute::Priority(v) => found.3 = Some(*v),
            _ => {}
        }
    }
    match found {
        (Some(value), Some(masked), Some(table), Some(PRIORITY)) => {
            Some(MarkRule { mark: value, mask: masked, table })
        }
        _ => None,
    }
}

/// A route in `table` to `dest/prefix` out of link `index`.
fn table_route(table: u32, (dest, prefix): (IpAddr, u8), index: u32) -> RouteMessage {
    let mut m = RouteMessage::default();
    m.header.address_family = ip_family(dest);
    m.header.destination_prefix_length = prefix;
    m.header.table = RouteHeader::RT_TABLE_UNSPEC;
    m.header.protocol = RouteProtocol::Static;
    m.header.kind = RouteType::Unicast;
    m.header.scope = RouteScope::Link;
    if prefix > 0 {
        m.attributes.push(RouteAttribute::Destination(route_address(dest)));
    }
    m.attributes.push(RouteAttribute::Oif(index));
    m.attributes.push(RouteAttribute::Table(table));
    m
}

impl Rtnl {
    /// Every rule, IPv4 and IPv6.
    fn all_rules(&mut self) -> io::Result<Vec<RuleMessage>> {
        let mut answers = Vec::new();
        for family in FAMILIES {
            let mut ask = RuleMessage::default();
            ask.header.family = family;
            let asked = RouteNetlinkMessage::GetRule(ask);
            answers.extend(self.exchange(asked, NLM_F_REQUEST | NLM_F_DUMP)?);
        }
        let mut found = Vec::new();
        for answer in answers {
            match answer.payload {
                NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRule(m)) => found.push(m),
                NetlinkPayload::Error(e) if e.code.is_some() => return Err(e.to_io()),
                _ => {}
            }
        }
        Ok(found)
    }

    /// Every rule of Korpis's priority, once each, whichever families it is in.
    pub(crate) fn mark_rules(&mut self) -> io::Result<Vec<MarkRule>> {
        let mut rules: Vec<MarkRule> = self.all_rules()?.iter().filter_map(read_rule).collect();
        rules.sort_by_key(|r| (r.mark, r.mask, r.table));
        rules.dedup();
        Ok(rules)
    }

    /// Every link with a rule at a link's priority, once each.
    pub(crate) fn link_rules(&mut self) -> io::Result<Vec<String>> {
        let mut names: Vec<String> = self.all_rules()?.iter().filter_map(read_link_rule).collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Adds link `name`'s rules in both families, unless they are there.
    pub(crate) fn add_link_rules(&mut self, name: &str) -> io::Result<()> {
        for m in FAMILIES.into_iter().flat_map(|f| link_messages(name, f)) {
            self.request(RouteNetlinkMessage::NewRule(m), true)?;
        }
        Ok(())
    }

    /// Removes link `name`'s rules in both families, if they are there.
    pub(crate) fn delete_link_rules(&mut self, name: &str) -> io::Result<()> {
        for m in FAMILIES.into_iter().flat_map(|f| link_messages(name, f)) {
            self.request(RouteNetlinkMessage::DelRule(m), false)?;
        }
        Ok(())
    }

    /// The source of every rule at the fallback priority.
    pub(crate) fn source_rules(&mut self) -> io::Result<Vec<IpAddr>> {
        Ok(self.all_rules()?.iter().filter_map(read_source_rule).collect())
    }

    /// Adds the fallback rule of `from` to `table`, unless it is there.
    pub(crate) fn add_source_rule(&mut self, from: IpAddr, table: u32) -> io::Result<()> {
        self.request(RouteNetlinkMessage::NewRule(source_message(from, table)), true).map(|_| ())
    }

    /// Removes the fallback rule of `from`, if it is there.
    pub(crate) fn delete_source_rule(&mut self, from: IpAddr, table: u32) -> io::Result<()> {
        self.request(RouteNetlinkMessage::DelRule(source_message(from, table)), false).map(|_| ())
    }

    /// Adds `r` in both families, unless it is there.
    pub(crate) fn add_mark_rule(&mut self, r: MarkRule) -> io::Result<()> {
        for family in FAMILIES {
            self.request(RouteNetlinkMessage::NewRule(rule_message(r, family)), true)?;
        }
        Ok(())
    }

    /// Removes `r` from both families, if it is there.
    pub(crate) fn delete_mark_rule(&mut self, r: MarkRule) -> io::Result<()> {
        for family in FAMILIES {
            self.request(RouteNetlinkMessage::DelRule(rule_message(r, family)), false)?;
        }
        Ok(())
    }

    /// The routes of `table`, IPv4 and IPv6: each destination with its prefix, and the link it
    /// leaves by.
    pub(crate) fn table_routes(&mut self, table: u32) -> io::Result<Vec<((IpAddr, u8), u32)>> {
        let mut found = Vec::new();
        for family in FAMILIES {
            found.extend(self.family_routes(table, family)?);
        }
        Ok(found)
    }

    /// The routes of `table` in `family`.
    fn family_routes(
        &mut self,
        table: u32,
        family: AddressFamily,
    ) -> io::Result<Vec<((IpAddr, u8), u32)>> {
        let mut ask = RouteMessage::default();
        ask.header.address_family = family;
        let any = if family == AddressFamily::Inet6 {
            IpAddr::from([0_u16; 8])
        } else {
            IpAddr::from([0_u8; 4])
        };
        let mut found = Vec::new();
        for answer in
            self.exchange(RouteNetlinkMessage::GetRoute(ask), NLM_F_REQUEST | NLM_F_DUMP)?
        {
            let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(r)) = answer.payload
            else {
                continue;
            };
            let mine = r.attributes.contains(&RouteAttribute::Table(table));
            let dest = r.attributes.iter().find_map(|a| match a {
                RouteAttribute::Destination(d) => super::links::destination(d),
                _ => None,
            });
            let oif = r.attributes.iter().find_map(|a| match a {
                RouteAttribute::Oif(i) => Some(*i),
                _ => None,
            });
            let prefix = r.header.destination_prefix_length;
            let dest = dest.or_else(|| (prefix == 0).then_some(any));
            if let (true, Some(d), Some(i)) = (mine, dest, oif) {
                found.push(((d, prefix), i));
            }
        }
        Ok(found)
    }

    /// Routes `dest/prefix` in `table` out of link `index`, replacing a route there to it.
    pub(crate) fn put_table_route(
        &mut self,
        table: u32,
        dest: (IpAddr, u8),
        index: u32,
    ) -> io::Result<()> {
        let message = RouteNetlinkMessage::NewRoute(table_route(table, dest, index));
        self.request_with(message, NLM_F_CREATE | NLM_F_REPLACE).map(|_| ())
    }

    /// Removes `table`'s route to `dest/prefix` out of link `index`, if it is there.
    pub(crate) fn delete_table_route(
        &mut self,
        table: u32,
        dest: (IpAddr, u8),
        index: u32,
    ) -> io::Result<()> {
        let message = RouteNetlinkMessage::DelRoute(table_route(table, dest, index));
        match self.request(message, false)? {
            Answer::Done | Answer::Missing | Answer::Exists => Ok(()),
        }
    }
}
