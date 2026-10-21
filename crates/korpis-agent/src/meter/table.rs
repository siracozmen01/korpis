// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the host's table counted of a workload's link (§7 of docs/07-networking.md), as the
//! meter reads it: its egress, and each endpoint whose accounting is on, by the endpoint's name.
//! The helper names each counter for the link and the port; the endpoint's name is the agent's,
//! from the assignment, since a record says which of a workload's services carried what (§2 of
//! docs/15-observability.md).

use crate::reconcile::network::transport;
use korpis_core::workload::network::host_link;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;

/// Each counter the helper listed, by name: its bytes and packets.
pub type Listed = BTreeMap<String, (u64, u64)>;

/// `counters` by name.
#[must_use]
pub fn listed(counters: Vec<helper::Counter>) -> Listed {
    counters.into_iter().map(|c| (c.name, (c.bytes, c.packets))).collect()
}

/// What `listed` counted of `workload`'s link, for its `endpoints`; none where its link has no
/// counters, its network not made or already released.
#[must_use]
pub fn counted(
    workload: &str,
    endpoints: &[pb::Endpoint],
    listed: &Listed,
) -> Option<agent::MeterCounters> {
    let link = host_link(workload);
    let (egress, _) = listed.get(&format!("{link}_egress"))?;
    let mut counters = agent::MeterCounters { egress_bytes: Some(*egress), ..Default::default() };
    for e in endpoints.iter().filter(|e| e.accounting) {
        let t = match transport(e) {
            helper::Transport::TRANSPORT_UDP => "udp",
            _ => "tcp",
        };
        let port = |dir: &str| listed.get(&format!("{link}_{t}_{}_{dir}", e.container_port));
        let (Some(received), Some(sent)) = (port("in"), port("out")) else { continue };
        counters.endpoints.push(agent::EndpointCounters {
            endpoint: e.name.clone(),
            received_bytes: received.0,
            received_packets: received.1,
            sent_bytes: sent.0,
            sent_packets: sent.1,
            ..Default::default()
        });
    }
    counters.endpoints.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    Some(counters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workloads_egress_and_each_counted_endpoint_are_read_by_its_link_and_named() {
        let link = host_link("wl_a");
        let mut listed = Listed::new();
        listed.insert(format!("{link}_egress"), (9000, 30));
        listed.insert(format!("{link}_udp_25565_in"), (1200, 4));
        listed.insert(format!("{link}_udp_25565_out"), (4800, 6));
        listed.insert(format!("{}_egress", host_link("wl_b")), (5, 1));
        let endpoint = |name: &str, protocol: pb::EndpointProtocol, accounting| pb::Endpoint {
            name: name.into(),
            protocol: protocol.into(),
            container_port: 25565,
            accounting,
            ..Default::default()
        };
        let game = endpoint("game", pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP, true);
        let rcon = endpoint("rcon", pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP, true);
        let quiet = endpoint("quiet", pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP, false);
        let c = counted("wl_a", &[game, rcon, quiet], &listed).unwrap();
        assert_eq!(c.egress_bytes, Some(9000));
        let names: Vec<&str> = c.endpoints.iter().map(|e| e.endpoint.as_str()).collect();
        assert_eq!(names, ["game"], "not yet counted, and not counted at all, are left out");
        let game = &c.endpoints[0];
        assert_eq!((game.received_bytes, game.sent_bytes, game.sent_packets), (1200, 4800, 6));
        assert!(counted("wl_c", &[], &listed).is_none(), "no network, nothing read");
    }
}
