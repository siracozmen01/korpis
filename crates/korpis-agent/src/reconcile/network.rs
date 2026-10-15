// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, made before its runtime object is created and released once the
//! workload has left the node with nothing of it left here (§4.1 of docs/07-networking.md).

use super::Reconciler;
use super::act::Failure;
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;

/// What a protocol rides on.
fn transport(p: &pb::Endpoint) -> helper::Transport {
    match p.protocol.as_known() {
        Some(
            pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
            | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
        ) => helper::Transport::TRANSPORT_UDP,
        _ => helper::Transport::TRANSPORT_TCP,
    }
}

/// The ports inside the workload its endpoints listen on, which its firewall accepts, and the
/// ports of the node forwarded to them (§3 of docs/07-networking.md). An endpoint of exposure
/// `none` is not reachable, so its port stays closed.
fn ports(endpoints: &[pb::Endpoint]) -> (Vec<helper::Port>, Vec<helper::Forward>) {
    let reachable =
        endpoints.iter().filter(|e| e.exposure.as_known() != Some(pb::Exposure::EXPOSURE_NONE));
    let inbound = reachable
        .map(|e| helper::Port {
            transport: transport(e).into(),
            port: e.container_port,
            ..Default::default()
        })
        .collect();
    let forwards = endpoints
        .iter()
        .filter_map(|e| {
            let node_port = e.address.as_option()?.port;
            Some(helper::Forward {
                transport: transport(e).into(),
                node_port,
                container_port: e.container_port,
                ..Default::default()
            })
        })
        .collect();
    (inbound, forwards)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Makes `a`'s workload's network and has `spec` join it, where the node gives networks;
    /// its namespaces map the uid range the assignment gives it.
    pub(super) async fn network(
        &mut self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        let Some(net) = self.wiring.network.as_mut() else { return Ok(()) };
        let assigned = net
            .addresses
            .assign(&a.workload, net.pools)
            .map_err(|e| Failure(format!("the workload's addresses: {e}")))?;
        let mut request = net.request(&a.workload, assigned);
        (request.uid_base, request.uid_count) = (a.uid_base, a.uid_count);
        (request.inbound, request.forwards) = ports(&a.endpoints);
        let made = self.host.ensure_network(&request).await?;
        spec.namespaces = made.namespaces;
        Ok(())
    }

    /// Releases the network of every workload holding addresses that the set no longer holds
    /// and nothing tracks; one that cannot be released now is tried again at the next pass.
    pub(super) async fn release_networks(&mut self, held: &HeldSet) {
        let Some(net) = self.wiring.network.as_ref() else { return };
        let leaving: Vec<String> = net
            .addresses
            .workloads()
            .filter(|w| !held.assignments.contains_key(*w) && !self.tracked.contains_key(*w))
            .cloned()
            .collect();
        for workload in leaving {
            if let Err(e) = self.host.release_network(&workload).await {
                tracing::warn!(%workload, error = %e, "a network could not be released yet");
                continue;
            }
            let freed = self.wiring.network.as_mut().map(|n| n.addresses.release(&workload));
            if let Some(Err(e)) = freed {
                tracing::error!(%workload, error = %e, "the addresses could not be kept");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(
        protocol: pb::EndpointProtocol,
        exposure: pb::Exposure,
        port: Option<u32>,
    ) -> pb::Endpoint {
        pb::Endpoint {
            protocol: protocol.into(),
            container_port: 25565,
            exposure: exposure.into(),
            address: port.map(|port| pb::Address { port, ..Default::default() }).into(),
            ..Default::default()
        }
    }

    #[test]
    fn declared_ports_are_accepted_and_held_node_ports_forwarded_over_their_transport() {
        let endpoints = [
            endpoint(
                pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
                pb::Exposure::EXPOSURE_NODE_PORT,
                Some(30001),
            ),
            endpoint(
                pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP,
                pb::Exposure::EXPOSURE_OVERLAY_ONLY,
                None,
            ),
            endpoint(
                pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP,
                pb::Exposure::EXPOSURE_NONE,
                None,
            ),
        ];
        let (inbound, forwards) = ports(&endpoints);
        let transports: Vec<_> = inbound.iter().map(|p| p.transport.as_known()).collect();
        assert_eq!(
            transports,
            [Some(helper::Transport::TRANSPORT_UDP), Some(helper::Transport::TRANSPORT_TCP)],
            "an endpoint of exposure none stays closed"
        );
        assert_eq!(forwards.len(), 1);
        assert_eq!((forwards[0].node_port, forwards[0].container_port), (30001, 25565));
        assert_eq!(forwards[0].transport.as_known(), Some(helper::Transport::TRANSPORT_UDP));
    }
}
