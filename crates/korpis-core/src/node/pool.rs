// SPDX-License-Identifier: AGPL-3.0-or-later

//! Each node's workload pool (§9.1 of docs/07-networking.md). The mesh routes workloads'
//! addresses between nodes rather than translating them, so no two nodes may share a pool, and
//! only the control plane sees every node: it gives each node a /22 of the installation's range
//! when it enrolls, the lowest one no node holds.

use ipnet::Ipv4Net;

/// A unique local /64 for the node `node` names, as the control plane names it: `fd`, then 40
/// bits of a hash of its identifier as RFC 4193's global identifier, then subnet zero. Derived
/// rather than random so that it survives the agent's state, different for each node so two
/// nodes' workloads never share one, and derivable by the control plane, which so knows it
/// without being told (§4.1 and §9.1 of docs/07-networking.md).
#[cfg(feature = "mesh")]
#[must_use]
pub fn unique_local(node: &str) -> ipnet::Ipv6Net {
    use sha2::Digest as _;
    let hash = sha2::Sha256::digest(node.as_bytes());
    let mut octets = [0_u8; 16];
    octets[0] = 0xfd;
    for (to, from) in octets.iter_mut().skip(1).zip(hash.iter().take(5)) {
        *to = *from;
    }
    ipnet::Ipv6Net::new(std::net::Ipv6Addr::from(octets), 64).unwrap_or_default()
}

/// The installation's range, unless `korpis-server --overlay-range` names another.
pub const DEFAULT_RANGE: &str = "10.48.0.0/12";

/// The prefix length of the pool each node is given: 1,022 workloads.
pub const POOL_PREFIX: u8 = 22;

/// Whether `range` can be cut into pools: a network no longer than a pool.
#[must_use]
pub fn holds_pools(range: Ipv4Net) -> bool {
    range.prefix_len() <= POOL_PREFIX && range.network() == range.addr()
}

/// The lowest pool of `range` that overlaps none of `taken`; none once every one is.
#[must_use]
pub fn lowest_free(range: Ipv4Net, taken: &[Ipv4Net]) -> Option<Ipv4Net> {
    if !holds_pools(range) {
        return None;
    }
    range
        .subnets(POOL_PREFIX)
        .ok()?
        .find(|p| !taken.iter().any(|t| t.contains(&p.network()) || p.contains(&t.network())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    fn net(s: &str) -> Ipv4Net {
        s.parse().unwrap()
    }

    #[test]
    fn each_node_is_given_the_lowest_pool_no_other_holds_and_none_once_the_range_is_full() {
        let range = net(DEFAULT_RANGE);
        assert_eq!(lowest_free(range, &[]), Some(net("10.48.0.0/22")));
        let taken = [net("10.48.0.0/22"), net("10.48.8.0/22")];
        assert_eq!(lowest_free(range, &taken), Some(net("10.48.4.0/22")), "a freed gap first");
        let wide = [net("10.48.0.0/16")];
        assert_eq!(lowest_free(range, &wide), Some(net("10.49.0.0/22")), "an operator's wider one");
        assert_eq!(lowest_free(net("10.48.0.0/22"), &[net("10.48.0.0/22")]), None);
        assert_eq!(lowest_free(net("10.48.0.0/23"), &[]), None, "a range smaller than a pool");
        assert!(!holds_pools(net("10.48.1.0/12")), "not a network's first address");
    }
}
