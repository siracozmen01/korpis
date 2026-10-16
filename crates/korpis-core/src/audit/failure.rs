// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failed attempts to authenticate (§10 of docs/10-api.md): a credential presented that opened
//! nothing. A failure is charged to its source, the address it came from, or the /64 of an IPv6
//! address, since one host holds a whole /64; and it is recorded as a denial is, coalesced by
//! source, kind, and target, the actor [`Principal::Anonymous`](crate::principal::Principal).

use std::net::IpAddr;

/// What was presented, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// An API token, a workload credential, or a share link, presented to the API.
    Credential,
    /// A node's `Hello`, opening its session.
    Hello,
    /// An enrollment token, joined by a node.
    Join,
    /// An enrollment token, joined by a relay.
    RelayJoin,
}

impl Attempt {
    /// The action its failures are recorded as.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Credential => "credential.present",
            Self::Hello => "node.hello",
            Self::Join => "node.join",
            Self::RelayJoin => "relay.join",
        }
    }

    /// The attempt whose failures are recorded as `action`.
    #[must_use]
    pub fn from_action(action: &str) -> Option<Self> {
        [Self::Credential, Self::Hello, Self::Join, Self::RelayJoin]
            .into_iter()
            .find(|a| a.action() == action)
    }
}

/// The source a failure from `ip` is charged to: the address of an IPv4 client, the /64 of an
/// IPv6 one, `unknown` for a client whose address was not seen.
#[must_use]
pub fn source(ip: Option<IpAddr>) -> String {
    match ip.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(v4)) => format!("{v4}/32"),
        Some(IpAddr::V6(v6)) => {
            let prefix = u128::from(v6) & !(u128::MAX >> 64);
            format!("{}/64", std::net::Ipv6Addr::from(prefix))
        }
        None => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_an_ipv4_address_or_the_64_of_an_ipv6_one() {
        let at = |s: &str| source(Some(s.parse().unwrap()));
        assert_eq!(at("203.0.113.7"), "203.0.113.7/32");
        assert_eq!(at("::ffff:203.0.113.7"), "203.0.113.7/32");
        assert_eq!(at("2001:db8:1:2:3:4:5:6"), "2001:db8:1:2::/64");
        assert_eq!(source(None), "unknown");
    }
}
