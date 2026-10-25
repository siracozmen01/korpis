// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the mesh routes to a member (§9.1 of docs/07-networking.md, Finding 178 of
//! docs/23-walkthroughs.md). A member's pools become routes on every member related to it and
//! the only sources accepted from it, so they are what the control plane gave or can derive,
//! never what the node says: the IPv4 pool it was given at enrollment, or for a node enrolled
//! before pools were given the default every such node shares, and the unique local IPv6 prefix
//! derived from its identity. A routed IPv6 prefix of the operator's is routed by the operator's
//! own network and not through the mesh.

use super::Candidate;
use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use std::net::Ipv4Addr;

/// The pool of a node enrolled before pools were given (§4.1 of docs/07-networking.md).
#[must_use]
pub fn legacy_pool() -> Ipv4Net {
    Ipv4Net::new(Ipv4Addr::new(10, 47, 0, 0), 16).unwrap_or_default()
}

/// Whether `p` is a unique local prefix, which no operator routes and only a derivation gives.
fn unique_local(p: &Ipv6Net) -> bool {
    p.addr().octets()[0] & 0xfe == 0xfc
}

/// The pools of `c` the mesh routes to it, or why it is outside: a pool it reports that the
/// control plane did not give it would have every member related to it route that pool's
/// addresses, its own hosts' included, to it.
///
/// # Errors
/// The first pool reported that is neither given nor derived.
pub fn routed(c: &Candidate) -> Result<Vec<IpNet>, String> {
    let mut out = Vec::new();
    for p in &c.facts.pools {
        match p {
            IpNet::V4(v4) => {
                if *v4 != c.given.unwrap_or_else(legacy_pool) {
                    return Err(format!("it reports pool {v4}, which it was not given"));
                }
                out.push(*p);
            }
            IpNet::V6(v6) if *v6 == c.derived_ipv6 => out.push(*p),
            IpNet::V6(v6) if unique_local(v6) => {
                return Err(format!("it reports pool {v6}, which its identity does not derive"));
            }
            // An operator's routed prefix: its own network routes it, not the mesh.
            IpNet::V6(_) => {}
        }
    }
    Ok(out)
}
