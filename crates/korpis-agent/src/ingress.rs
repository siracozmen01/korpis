// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress (§3.2 of docs/07-networking.md): the routes its assignments carry, made
//! into what the proxy serves, and the proxy itself, which the agent has the helper start and
//! then feeds and watches.

mod supervise;

use crate::held::HeldSet;
use crate::network::Addresses;
use korpis_proto::korpis::ingress::v1 as ingress;
use korpis_proto::korpis::v1 as pb;

pub use supervise::Supervisor;

/// How the proxy serves `e`: terminated and forwarded over HTTP or within TLS, or passed through.
fn mode(e: &pb::Endpoint) -> ingress::RouteMode {
    let passthrough = e
        .tls
        .as_option()
        .is_some_and(|t| t.source.as_known() == Some(pb::TlsSource::TLS_SOURCE_PASSTHROUGH));
    match e.protocol.as_known() {
        _ if passthrough => ingress::RouteMode::ROUTE_MODE_PASSTHROUGH,
        Some(pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTPS) => ingress::RouteMode::ROUTE_MODE_HTTPS,
        _ => ingress::RouteMode::ROUTE_MODE_HTTP,
    }
}

/// What the proxy serves for the assignments `held`, each workload reached at its address in
/// `addresses`: one route for each of their `ingress` endpoints. A workload with no address yet
/// has no route until its network is made. A residual assignment runs nothing, so routes nothing.
#[must_use]
pub fn config(held: &HeldSet, addresses: &Addresses) -> ingress::IngressConfig {
    let mut routes = Vec::new();
    for (workload, a) in held.assignments.iter().filter(|(_, a)| !a.residual) {
        let Some(assigned) = addresses.of(workload) else { continue };
        let ingress = a.endpoints.iter().filter(|e| {
            e.exposure.as_known() == Some(pb::Exposure::EXPOSURE_INGRESS) && !e.domains.is_empty()
        });
        routes.extend(ingress.map(|e| ingress::Route {
            names: e.domains.clone(),
            backend: format!("{}:{}", assigned.ipv4, e.container_port),
            mode: mode(e).into(),
            workload: workload.clone(),
            endpoint: e.name.clone(),
            ..Default::default()
        }));
    }
    ingress::IngressConfig { routes, ..Default::default() }
}

#[cfg(test)]
mod tests;
