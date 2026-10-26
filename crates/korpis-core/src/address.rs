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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Routed to this one node.
    Node(Uuid),
    /// Held by a member of this edge group (§3.1 of docs/07-networking.md): on a link the
    /// members share where no router is named, and otherwise moved to the holding member by the
    /// `address_router` provider of this name the pool's organization sees.
    Edge {
        /// The group.
        group: Uuid,
        /// The router, for an edge pool whose routing is not static.
        router: Option<String>,
    },
    /// Moved to whichever node holds one by the `address_router` provider of this name that
    /// the pool's organization sees.
    Cluster(String),
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
    /// A `node` pool's routing is static, a `cluster` pool's is not, and an `edge` pool's is
    /// static exactly where it names no router.
    #[error(
        "a node pool is routed statically, a cluster pool by its router, and an edge pool \
         statically on its members' link or by the router it names"
    )]
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
    /// The router is not a name a provider may have.
    #[error("{0:?} is not a provider's name")]
    Router(String),
}

impl AddressPool {
    /// Checks what the pool itself says.
    ///
    /// # Errors
    /// The first thing it says that could not hold.
    pub fn validate(&self) -> Result<(), PoolError> {
        let r#static = self.routing == Routing::Static;
        let as_routed = match &self.scope {
            Scope::Node(_) => r#static,
            Scope::Cluster(_) => !r#static,
            Scope::Edge { router, .. } => router.is_none() == r#static,
        };
        if !as_routed {
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
        match &self.scope {
            Scope::Cluster(router) | Scope::Edge { router: Some(router), .. }
                if !crate::extension::slug(router) =>
            {
                Err(PoolError::Router(router.clone()))
            }
            _ => Ok(()),
        }
    }

    /// Whether its addresses may be held on `node`, which belongs to `organization`.
    #[must_use]
    pub fn reaches(&self, node: Uuid, organization: Uuid) -> bool {
        organization == self.organization
            && match self.scope {
                Scope::Node(n) => n == node,
                Scope::Cluster(_) => true,
                Scope::Edge { .. } => false,
            }
    }

    /// The edge group whose members hold its addresses, for an edge pool.
    #[must_use]
    pub fn group(&self) -> Option<Uuid> {
        match self.scope {
            Scope::Edge { group, .. } => Some(group),
            _ => None,
        }
    }

    /// The `address_router` provider that moves its addresses, by name, where one does.
    #[must_use]
    pub fn router(&self) -> Option<&str> {
        match &self.scope {
            Scope::Cluster(r) | Scope::Edge { router: Some(r), .. } => Some(r),
            _ => None,
        }
    }

    /// Every address of it that may be given, lowest first: neither reserved nor the network's
    /// first or last, which some routers drop even where the network is routed to the node.
    pub fn addresses(&self) -> impl Iterator<Item = IpAddr> + '_ {
        // IPv6's hosts include the subnet's first address, its routers' anycast address.
        let ends = match self.cidr {
            IpNet::V6(n) if n.prefix_len() < 127 => {
                vec![self.cidr.network(), self.cidr.broadcast()]
            }
            _ => Vec::new(),
        };
        self.cidr.hosts().filter(move |a| !self.reserved.contains(a) && !ends.contains(a))
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

/// What a router is asked to make so of one address (§3.7 of docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The address.
    pub address: std::net::Ipv4Addr,
    /// The node that is to hold it; none where it is to be routed nowhere.
    pub node: Option<Uuid>,
    /// The node's advertised addresses, which the network routes it through.
    pub next_hops: Vec<IpAddr>,
}

/// The address a workload holds, as the control plane keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holding {
    /// The address.
    pub address: IpAddr,
    /// Its pool.
    pub pool: Uuid,
    /// Whether its pool is a cluster pool, whose router moves it.
    pub cluster: bool,
    /// For a cluster pool's address, the node its router last said it routes it to.
    pub routed: Option<Uuid>,
    /// Why the router's last answer was not that it routes it, while it is asked again.
    pub failing: Option<String>,
}

impl Holding {
    /// Whether it waits on its router to be routed to `node`, where its holder is placed: a
    /// cluster pool's address its router has not said it routes there.
    #[must_use]
    pub fn waits_on_router(&self, node: Option<Uuid>) -> bool {
        self.cluster && node.is_some() && self.routed != node
    }
}

/// A cluster pool's address whose router is to be asked, as one control plane took it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueRoute {
    /// The address.
    pub address: std::net::Ipv4Addr,
    /// The pool's organization, whose providers name its router.
    pub organization: Uuid,
    /// The router, by its provider's name.
    pub router: String,
    /// The node its holder is placed on; none where no placed workload holds it.
    pub wanted: Option<Uuid>,
    /// How many calls have failed since one answered.
    pub attempts: u32,
}

/// How long after its `attempts`th failure in a row a router is asked again: ten seconds,
/// doubling, at most ten minutes, so a router back from an outage is asked within ten minutes.
#[must_use]
pub fn route_backoff(attempts: u32) -> Duration {
    let seconds = 10u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(16));
    Duration::from_secs(seconds).min(Duration::from_secs(600))
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
