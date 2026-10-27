// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, made before its runtime object is created and released once the
//! workload has left the node with nothing of it left here (§4.1 of docs/07-networking.md).

use super::Reconciler;
use super::act::Failure;
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;

/// What a protocol rides on.
pub(crate) fn transport(p: &pb::Endpoint) -> helper::Transport {
    match p.protocol.as_known() {
        Some(
            pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
            | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
        ) => helper::Transport::TRANSPORT_UDP,
        _ => helper::Transport::TRANSPORT_TCP,
    }
}

/// The ports inside the workload its endpoints listen on, which its firewall accepts, and the
/// ports of the node forwarded to them (§3 of docs/07-networking.md), each with its own limits
/// and whether it is counted (§7 there). An endpoint of exposure
/// `none` is not reachable, so its port stays closed. Only `node_port` and `node_bound` hold a
/// port of the node; an `ingress` endpoint's address is the proxy's, on 443, and the proxy
/// reaches the workload at its own address. A port is reached from outside the node at the
/// workload's own address only where its exposure is `node_port`, `node_bound`, or
/// `dedicated_ip`; any other, from outside, only through what the node translates or proxies
/// (§4 there).
fn ports(endpoints: &[pb::Endpoint]) -> (Vec<helper::Port>, Vec<helper::Forward>) {
    let reachable =
        endpoints.iter().filter(|e| e.exposure.as_known() != Some(pb::Exposure::EXPOSURE_NONE));
    let inbound = reachable
        .map(|e| {
            let shaping = e.shaping.as_option();
            helper::Port {
                transport: transport(e).into(),
                port: e.container_port,
                egress_limit: shaping.map_or(0, |s| s.egress_bits_per_sec),
                ingress_limit: shaping.map_or(0, |s| s.ingress_bits_per_sec),
                accounted: e.accounting,
                outside: matches!(
                    e.exposure.as_known(),
                    Some(
                        pb::Exposure::EXPOSURE_NODE_PORT
                            | pb::Exposure::EXPOSURE_NODE_BOUND
                            | pb::Exposure::EXPOSURE_DEDICATED_IP
                    )
                ),
                ..Default::default()
            }
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
    /// its namespaces map the uid range the assignment gives it. A microVM's guest, which holds
    /// its addresses itself, is given them as well.
    pub(super) async fn network(
        &mut self,
        a: &agent::Assignment,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        let Some((asked, made)) = self.ensure_network(a).await? else { return Ok(()) };
        if asked.tap {
            spec.addresses = vec![format!("{}/32", asked.ipv4)];
            if made.ipv6 && !asked.ipv6.is_empty() {
                spec.addresses.push(format!("{}/128", asked.ipv6));
            }
        }
        spec.namespaces = made.namespaces;
        self.serve_resolver(&a.workload).await;
        Ok(())
    }

    /// What the helper would be asked to make `a`'s workload's network now, allocating its
    /// addresses if it has none, with a tap where its driver's workloads hold one; `None` where
    /// the node gives no networks.
    fn network_request(
        &mut self,
        a: &agent::Assignment,
    ) -> Result<Option<helper::EnsureNetworkRequest>, Failure> {
        let dependencies = self.dependency_destinations(a);
        let tap = self.wiring.driver_kinds.tap(super::volume::driver_of(a));
        let Some(net) = self.wiring.network.as_mut() else { return Ok(None) };
        let assigned = net
            .addresses
            .assign(&a.workload, net.pools)
            .map_err(|e| Failure(format!("the workload's addresses: {e}")))?;
        let mut request = net.request(&a.workload, assigned);
        (request.uid_base, request.uid_count) = (a.uid_base, a.uid_count);
        (request.inbound, request.forwards) = ports(&a.endpoints);
        let body = a.target.as_option().and_then(|t| t.body.as_option());
        super::policy::apply(&mut request, body, &a.peers)?;
        super::policy::spent(&mut request, a.shaping.as_option());
        request.opened = dependencies;
        request.tap = tap;
        request.dedicated = a
            .dedicated
            .as_option()
            .map(|d| helper::DedicatedAddress {
                address: d.address.clone(),
                interface: d.interface.clone(),
                ..Default::default()
            })
            .into();
        Ok(Some(request))
    }

    /// Asks the helper to make `a`'s workload's network what `a` says now: what was asked, and
    /// what was made, or `None` where the node gives no networks.
    pub(super) async fn ensure_network(
        &mut self,
        a: &agent::Assignment,
    ) -> Result<Option<(helper::EnsureNetworkRequest, helper::EnsureNetworkResponse)>, Failure>
    {
        let Some(request) = self.network_request(a)? else { return Ok(None) };
        let made = self.host.ensure_network(&request).await?;
        if let Some(n) = self.wiring.network.as_mut() {
            n.asked.insert(a.workload.clone(), request.clone());
        }
        Ok(Some((request, made)))
    }

    /// Makes every running workload's network again where what it would be asked differs from
    /// what it was last asked: the destinations its dependencies open, its spent egress budget,
    /// its policy; and once after the agent starts, which has asked nothing yet, so a node's
    /// table written anew by an upgraded helper holds every running workload's chains again.
    /// One that cannot be made now is tried again at the next pass.
    pub(super) async fn refresh_networks(&mut self, held: &crate::held::HeldSet) {
        let running: Vec<agent::Assignment> = held
            .assignments
            .values()
            .filter(|a| self.tracked.get(&a.workload).is_some_and(|t| t.runtime.is_some()))
            .cloned()
            .collect();
        for a in running {
            let request = match self.network_request(&a) {
                Ok(Some(request)) => request,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(workload = %a.workload, error = %e.0, "its network cannot be made as asked");
                    continue;
                }
            };
            let asked = self.wiring.network.as_ref().and_then(|n| n.asked.get(&a.workload));
            if asked == Some(&request) {
                continue;
            }
            if let Err(e) = self.host.ensure_network(&request).await {
                tracing::warn!(workload = %a.workload, error = %e, "its network could not be made again yet");
                continue;
            }
            if let Some(n) = self.wiring.network.as_mut() {
                n.asked.insert(a.workload.clone(), request);
            }
        }
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

    /// Publishes what the node's ingress proxy serves for the set held and for its edge groups
    /// (§3.1 of docs/07-networking.md), when it changed; and says once the proxy has been given
    /// the groups' challenges.
    pub(super) async fn publish_ingress(&mut self, held: &HeldSet) {
        let (Some(publish), Some(net)) = (&self.wiring.ingress, &self.wiring.network) else {
            return;
        };
        let edges = self.wiring.edges.as_ref().map(|e| e.borrow().clone()).unwrap_or_default();
        let certificates = &self.wiring.certificates;
        let mut wanted = crate::ingress::wanted(held);
        let config = crate::ingress::config(held, &net.addresses, Vec::new());
        let mut config = crate::ingress::with_edges(config, (&edges, held), &mut wanted);
        for (id, revision) in &wanted {
            certificates.want(id, *revision);
        }
        config.certificates = certificates.serve(&wanted.into_keys().collect());
        publish.send_if_modified(|served| {
            let changed = *served != config;
            if changed {
                *served = config;
            }
            changed
        });
        let proofs = crate::ingress::held_proofs(&edges);
        if self.proofs.as_ref() != Some(&proofs) {
            let up = agent::AgentMessage {
                message: Some(Up::EdgeChallengesHeld(Box::new(proofs.clone()))),
                ..Default::default()
            };
            if self.wiring.outbound.send(up).await.is_ok() {
                self.proofs = Some(proofs);
            }
        }
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

    /// Serves the API's relay to every workload the set marks as reaching it (an installed
    /// extension's, or one holding a grant in force) and that has a runtime object here, its own
    /// or a run's, and withdraws it from every other: an installation or a grant can begin or end
    /// while its workload runs, and a restarted agent serves it again (§9.5 of
    /// docs/08-identity.md).
    pub(super) fn serve_relays(&self, held: &HeldSet) {
        let Some(identity) = &self.wiring.identity else { return };
        for a in held.assignments.values().filter(|a| !a.residual) {
            let built = self.tracked.get(&a.workload).is_some_and(|t| {
                t.runtime.is_some() || t.runs.values().any(|r| !r.runtime.is_empty())
            });
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
                n.asked.remove(&workload);
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
        let outside: Vec<bool> = inbound.iter().map(|p| p.outside).collect();
        assert_eq!(outside, [true, false], "an overlay_only port is reached by workloads alone");
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
        assert!(!inbound[1].outside, "and to no one outside the node past it");
        assert_eq!(forwards.iter().map(|f| f.node_port).collect::<Vec<_>>(), [30001]);
    }
}
