// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's probe of each member through its forwarding path (§3.1 of
//! docs/07-networking.md, Finding 189). A probe is one UDP datagram to the probe port of a
//! member's own address, or of an address it holds: a nonce, the group, and the address probed.
//! The member translates it into the GRE link of a node the group forwards to, whose agent
//! answers with its node key's signature over a message bound to the purpose, the group, the
//! address, itself, and the nonce, and its reply comes back through the same link and the
//! member's translation. An answer so proves the whole path, both ways.

use std::fmt::Write as _;
use std::net::{IpAddr, Ipv6Addr};
use std::time::SystemTime;
use uuid::Uuid;

/// The length of a nonce.
pub const NONCE: usize = 32;
/// The length of a probe: the nonce, the group, and the address probed, as IPv6.
pub const REQUEST: usize = NONCE + 16 + 16;
/// The length of an answer: an Ed25519 signature.
pub const ANSWER: usize = 64;

/// The datagram probing `address` of `group` with `nonce`.
#[must_use]
pub fn request(nonce: &[u8; NONCE], group: Uuid, address: IpAddr) -> Vec<u8> {
    let as_v6 = match address {
        IpAddr::V4(a) => a.to_ipv6_mapped(),
        IpAddr::V6(a) => a,
    };
    [&nonce[..], group.as_bytes(), &as_v6.octets()].concat()
}

/// A probe read: its nonce, its group, and the address it probed.
#[must_use]
pub fn read_request(bytes: &[u8]) -> Option<([u8; NONCE], Uuid, IpAddr)> {
    if bytes.len() != REQUEST {
        return None;
    }
    let nonce: [u8; NONCE] = bytes.get(..NONCE)?.try_into().ok()?;
    let group = Uuid::from_slice(bytes.get(NONCE..NONCE + 16)?).ok()?;
    let octets: [u8; 16] = bytes.get(NONCE + 16..)?.try_into().ok()?;
    Some((nonce, group, Ipv6Addr::from(octets).to_canonical()))
}

/// What node `node`, a wire identifier, signs to answer the probe of `address` of `group` with
/// `nonce`: in the form of the reachability probe's answer, under its own purpose, so an answer
/// is good for this probe and nothing else.
#[must_use]
pub fn answer_message(group: Uuid, address: IpAddr, node: &str, nonce: &[u8]) -> Vec<u8> {
    let mut s = format!("korpis.agent.v1.EdgeProbe|{group}|{address}|{node}|");
    for b in nonce {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s.into_bytes()
}

/// What the prober has measured of one address: how many probes it has missed in a row, and
/// whether it passes, none until it has answered once or missed as many as are allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measured {
    /// Probes missed in a row.
    pub misses: u32,
    /// Whether it passes.
    pub passes: Option<bool>,
}

impl Measured {
    /// Takes one probe's outcome, `allowed` misses in a row making it fail; whether it changed
    /// whether it passes.
    pub fn take(&mut self, answered: bool, allowed: u32) -> bool {
        let before = self.passes;
        if answered {
            self.misses = 0;
            self.passes = Some(true);
        } else {
            self.misses = self.misses.saturating_add(1);
            if self.misses >= allowed.max(1) {
                self.passes = Some(false);
            }
        }
        before != self.passes
    }
}

/// Where a routed edge pool's router last said it routes an address, and why it has not
/// routed it to its holder (§3.1 of docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRoute {
    /// The router, by its provider's name.
    pub router: String,
    /// The node it last said it routes the address to; none until it has, or where it routes it
    /// nowhere.
    pub routed: Option<Uuid>,
    /// Why its last answer was not that it routes it, while it is asked again.
    pub failing: Option<String>,
}

/// What a group's status shows of an address: its lease, its own probe through its holder, and
/// its route where its pool's router moves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressStatus {
    /// The lease.
    pub hold: super::lease::Hold,
    /// Whether it answered its last probes through its holder; none until probed.
    pub answering: Option<bool>,
    /// When that was last recorded.
    pub probed_at: Option<SystemTime>,
    /// Its route; none where its pool is on a link the members share.
    pub route: Option<EdgeRoute>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_probe_names_its_group_and_address_and_a_member_fails_only_after_its_misses() {
        let group = Uuid::from_u128(7);
        let address: IpAddr = "198.51.100.2".parse().unwrap();
        let sent = request(&[9; NONCE], group, address);
        assert_eq!(sent.len(), REQUEST);
        assert_eq!(read_request(&sent), Some(([9; NONCE], group, address)));
        assert!(read_request(sent.get(..REQUEST - 1).unwrap()).is_none());
        let signed = answer_message(group, address, "nd_a", &[1, 2]);
        assert!(String::from_utf8(signed).unwrap().ends_with("|198.51.100.2|nd_a|0102"));

        let mut m = Measured::default();
        assert!(!m.take(false, 3) && !m.take(false, 3), "two misses decide nothing yet");
        assert!(m.take(false, 3) && m.passes == Some(false), "the third does");
        assert!(m.take(true, 3) && m.passes == Some(true), "one answer and it passes again");
    }
}
