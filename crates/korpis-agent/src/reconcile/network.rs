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
/// `none` is not reachable, so its port stays closed. Only `node_port` and `node_bound` hold a
/// port of the node; an `ingress` endpoint's address is the proxy's, on 443, and the proxy
/// reaches the workload at its own address.
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
        .filter(|e| {
            matches!(
                e.exposure.as_known(),
                Some(pb::Exposure::EXPOSURE_NODE_PORT | pb::Exposure::EXPOSURE_NODE_BOUND)
            )
        })
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
        let Some(made) = self.ensure_network(a).await? else { return Ok(()) };
        spec.namespaces = made.namespaces;
        self.serve_resolver(&a.workload).await;
        Ok(())
    }

    /// Asks the helper to make `a`'s workload's network what `a` says now, allocating its
    /// addresses if it has none; `None` where the node gives no networks.
    pub(super) async fn ensure_network(
        &mut self,
        a: &agent::Assignment,
    ) -> Result<Option<helper::EnsureNetworkResponse>, Failure> {
        let dependencies = self.dependency_destinations(a);
        let Some(net) = self.wiring.network.as_mut() else { return Ok(None) };
        let assigned = net
            .addresses
            .assign(&a.workload, net.pools)
            .map_err(|e| Failure(format!("the workload's addresses: {e}")))?;
        let mut request = net.request(&a.workload, assigned);
        (request.uid_base, request.uid_count) = (a.uid_base, a.uid_count);
        (request.inbound, request.forwards) = ports(&a.endpoints);
        let body = a.target.as_option().and_then(|t| t.body.as_option());
        super::policy::apply(&mut request, body)?;
        request.opened.clone_from(&dependencies);
        let made = self.host.ensure_network(&request).await?;
        if let Some(n) = self.wiring.network.as_mut() {
            n.dependencies_opened.insert(a.workload.clone(), dependencies);
        }
        Ok(Some(made))
    }

    /// Serves `workload`'s resolver on the sockets the helper opens in its namespace, unless it
    /// is served; one that cannot be opened now is tried again at the next pass.
    async fn serve_resolver(&mut self, workload: &str) {
        let serving = self
            .wiring
            .network
            .as_ref()
            .is_some_and(|n| n.resolvers.get(workload).is_some_and(|t| !t.is_finished()));
        if serving || self.wiring.network.is_none() {
            return;
        }
        let bound = match self.host.open_resolver(workload).await {
            Ok(fds) => crate::resolver::Bound::from_attached(fds).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match bound {
            Ok(bound) => {
                let resolver = crate::resolver::Resolver::new("/etc/resolv.conf".into());
                let task = tokio::spawn(resolver.serve(bound));
                if let Some(n) = self.wiring.network.as_mut() {
                    n.resolvers.insert(workload.to_owned(), task);
                }
            }
            Err(e) => tracing::warn!(%workload, error = %e, "its resolver could not be served yet"),
        }
    }

    /// Publishes what the node's ingress proxy serves for the set held, when it changed.
    pub(super) fn publish_ingress(&self, held: &HeldSet) {
        let (Some(publish), Some(net)) = (&self.wiring.ingress, &self.wiring.network) else {
            return;
        };
        let config = crate::ingress::config(held, &net.addresses);
        publish.send_if_modified(|served| {
            let changed = *served != config;
            if changed {
                *served = config;
            }
            changed
        });
    }

    /// Serves the resolver of every workload holding a network the set holds, which is how a
    /// restarted agent answers its running workloads again.
    pub(super) async fn serve_resolvers(&mut self, held: &HeldSet) {
        let Some(net) = self.wiring.network.as_ref() else { return };
        let holding: Vec<String> = net
            .addresses
            .workloads()
            .filter(|w| held.assignments.contains_key(*w))
            .filter(|w| net.resolvers.get(*w).is_none_or(tokio::task::JoinHandle::is_finished))
            .cloned()
            .collect();
        for workload in holding {
            self.serve_resolver(&workload).await;
        }
    }

    /// Serves the API's relay to every workload the set marks as an installed extension's and
    /// that has a runtime object here, and withdraws it from every other: an installation can
    /// begin or end while its workload runs, and a restarted agent serves it again (§9.5 of
    /// docs/08-identity.md).
    pub(super) fn serve_relays(&self, held: &HeldSet) {
        let Some(identity) = &self.wiring.identity else { return };
        for a in held.assignments.values().filter(|a| !a.residual) {
            let built = self.tracked.get(&a.workload).is_some_and(|t| t.runtime.is_some());
            if let Err(e) = identity.relay(&a.workload, a.reaches_api && built) {
                tracing::warn!(workload = %a.workload, error = %e, "the API's relay could not be served");
            }
        }
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
            // Its sockets keep its namespace alive, so they are closed first.
            if let Some(task) =
                self.wiring.network.as_mut().and_then(|n| n.resolvers.remove(&workload))
            {
                task.abort();
            }
            if let Err(e) = self.host.release_network(&workload).await {
                tracing::warn!(%workload, error = %e, "a network could not be released yet");
                continue;
            }
            if let Some(n) = self.wiring.network.as_mut() {
                n.dependencies_opened.remove(&workload);
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

    #[test]
    fn an_ingress_endpoints_address_is_the_proxys_and_not_forwarded() {
        let http = pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP;
        let (inbound, forwards) = ports(&[
            endpoint(http, pb::Exposure::EXPOSURE_NODE_BOUND, Some(30001)),
            endpoint(http, pb::Exposure::EXPOSURE_INGRESS, Some(443)),
        ]);
        assert_eq!(inbound.len(), 2, "its port is open to the proxy");
        assert_eq!(forwards.iter().map(|f| f.node_port).collect::<Vec<_>>(), [30001]);
    }
}
