// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools and the dedicated addresses given from them (§3.7 and §4 of
//! docs/07-networking.md). A pool belongs to an organization and its addresses are held on that
//! organization's own nodes alone, since a node holding an address translates every connection to
//! it: a `node` pool's on its one node, a `cluster` pool's on whichever of them its router moves
//! one to. A workload with any `dedicated_ip` endpoint holds one IPv4 address, the lowest free one
//! of a pool reaching its node, and gives it back after the pool's quarantine.

use ipnet::IpNet;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a released address rests before it is given again, unless its pool says otherwise.
pub const DEFAULT_QUARANTINE: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a pool's addresses are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Routed to this one node.
    Node(Uuid),
    /// Held by an edge of the edge tier (§3.1 of docs/07-networking.md).
    Edge,
    /// Moved by this address router, an extension installation, to whichever node holds one.
    Cluster(Uuid),
}

/// How a pool's addresses reach the node that holds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// The network routes them to one node, which they never leave.
    Static,
    /// A router announces each over BGP from the node that holds it.
    Bgp,
    /// A router moves each through its provider's API.
    ProviderApi,
}

/// An address pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressPool {
    /// The pool.
    pub id: Uuid,
    /// The organization it belongs to, whose workloads and those below it it serves.
    pub organization: Uuid,
    /// Its addresses.
    pub cidr: IpNet,
    /// Where they are held.
    pub scope: Scope,
    /// How they reach their node.
    pub routing: Routing,
    /// The node's interface its addresses are answered for on, where they are on the node's
    /// link; empty where they are routed to the node.
    pub interface: String,
    /// How long a released address rests.
    pub quarantine: Duration,
    /// Addresses of it never given: a link's network, gateway, and broadcast addresses.
    pub reserved: Vec<IpAddr>,
    /// When it was made.
    pub created_at: SystemTime,
}

/// Why a pool is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    /// A `node` pool's routing is static, and only a `node` pool's is.
    #[error("a node pool's addresses are routed statically to it, and only a node pool's are")]
    Routing,
    /// A reserved address is outside the pool.
    #[error("reserved address {0} is not in the pool")]
    Reserved(IpAddr),
    /// The interface is not a name an interface may have.
    #[error("{0:?} is not an interface's name")]
    Interface(String),
    /// The pool holds no address that could be given.
    #[error("the pool holds no address to give")]
    Empty,
}

impl AddressPool {
    /// Checks what the pool itself says.
    ///
    /// # Errors
    /// The first thing it says that could not hold.
    pub fn validate(&self) -> Result<(), PoolError> {
        if matches!(self.scope, Scope::Node(_)) != (self.routing == Routing::Static) {
            return Err(PoolError::Routing);
        }
        if let Some(a) = self.reserved.iter().find(|a| !self.cidr.contains(*a)) {
            return Err(PoolError::Reserved(*a));
        }
        let name = self.interface.as_str();
        let named = name.len() < 16
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if !named {
            return Err(PoolError::Interface(name.to_owned()));
        }
        if self.addresses().next().is_none() {
            return Err(PoolError::Empty);
        }
        Ok(())
    }

    /// Whether its addresses may be held on `node`, which belongs to `organization`.
    #[must_use]
    pub fn reaches(&self, node: Uuid, organization: Uuid) -> bool {
        organization == self.organization
            && match self.scope {
                Scope::Node(n) => n == node,
                Scope::Cluster(_) => true,
                Scope::Edge => false,
            }
    }

    /// Every address of it that may be given, lowest first: neither reserved nor the network's
    /// first or last, which some routers drop even where the network is routed to the node.
    pub fn addresses(&self) -> impl Iterator<Item = IpAddr> + '_ {
        self.cidr.hosts().filter(|a| !self.reserved.contains(a))
    }
}

/// A dedicated address as its node holds it (§3.7 of docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dedicated {
    /// The address.
    pub address: std::net::Ipv4Addr,
    /// The node's interface it is answered for on, where its pool's addresses are on the node's
    /// link; empty where they are routed to the node.
    pub interface: String,
}

/// What holds an address now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// A workload holds it.
    Held(Uuid),
    /// No one does, since this instant.
    Released(SystemTime),
}

/// The lowest address of `pool` no workload holds and none released within its quarantine
/// before `now`, by what `taken` says of each.
#[must_use]
pub fn lowest_free(
    pool: &AddressPool,
    taken: &BTreeMap<IpAddr, Taken>,
    now: SystemTime,
) -> Option<IpAddr> {
    pool.addresses().find(|a| match taken.get(a) {
        None => true,
        Some(Taken::Held(_)) => false,
        Some(Taken::Released(at)) => {
            now.duration_since(*at).is_ok_and(|rested| rested >= pool.quarantine)
        }
    })
}

#[cfg(test)]
mod tests;
