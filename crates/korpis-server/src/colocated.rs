// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nodes this control plane replica shares its network with (§3.1 of docs/07-networking.md):
//! every [`EVERY`] it tries to bind to each mesh member's own addresses in the mesh, which
//! succeeds only where the address is its own, and records the nodes it could, for [`HOLD`]. An
//! edge group's probe answers through another node than those where it can, since such a node's
//! answer to this replica is delivered locally and never crosses the edge's link back.

use crate::api::Api;
use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// How often the replica looks.
const EVERY: Duration = Duration::from_secs(20);
/// How long what it found stands, so that what a replica that stopped found lapses.
const HOLD: Duration = Duration::from_secs(60);

/// Whether `address` is one of this process's network's own.
fn own(address: IpAddr) -> bool {
    UdpSocket::bind((address, 0)).is_ok()
}

/// Looks every [`EVERY`] and records what it finds; the task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let replica = api.providers().replica();
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let addresses = match api.app().mesh_addresses().await {
            Ok(addresses) => addresses,
            Err(e) => {
                tracing::warn!(error = %e, "the mesh's addresses could not be read");
                continue;
            }
        };
        let near: Vec<Uuid> = addresses
            .iter()
            .filter(|(_, m)| own(IpAddr::V4(m.v4)) || m.v6.is_some_and(|a| own(IpAddr::V6(a))))
            .map(|(node, _)| *node)
            .collect();
        if let Err(e) = api.app().record_colocated(replica, &near, HOLD).await {
            tracing::warn!(error = %e, "the nodes sharing this replica's network were not recorded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_this_network_s_own_only_where_it_may_be_bound() {
        assert!(own(IpAddr::from([127, 0, 0, 1])), "the loopback is");
        assert!(!own(IpAddr::from([192, 0, 2, 1])), "an address of the documentation's is not");
    }
}
