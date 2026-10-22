// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress (§3.2 of docs/07-networking.md): the routes its assignments carry, made
//! into what the proxy serves, and the proxy itself, which the agent has the helper start and
//! then feeds and watches.

mod certificates;
mod supervise;

use crate::held::HeldSet;
use crate::network::Addresses;
use korpis_proto::korpis::ingress::v1 as ingress;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;

pub use certificates::Certificates;
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

/// The certificates the ingress endpoints of `held` serve with, each at the revision their
/// assignment names: what the node must hold.
#[must_use]
pub fn wanted(held: &HeldSet) -> BTreeMap<String, i64> {
    let mut wanted = BTreeMap::new();
    let endpoints = held.assignments.values().filter(|a| !a.residual).flat_map(|a| &a.endpoints);
    for e in endpoints.filter(|e| !e.certificate.is_empty()) {
        let at = wanted.entry(e.certificate.clone()).or_insert(e.certificate_revision);
        *at = (*at).max(e.certificate_revision);
    }
    wanted
}

/// The HTTP-01 challenges the assignments `held` carry, which the proxy answers on port 80.
/// They are taken from the set the node holds, not from the reconciler's pass, so a challenge is
/// answered as soon as the set carrying it is held, however long a pass takes.
#[must_use]
pub fn challenges(held: &HeldSet) -> Vec<ingress::Challenge> {
    let mut out: Vec<ingress::Challenge> = held
        .assignments
        .values()
        .filter(|a| !a.residual)
        .flat_map(|a| &a.challenges)
        .map(|c| ingress::Challenge {
            token: c.token.clone(),
            key_authorization: c.key_authorization.clone(),
            ..Default::default()
        })
        .collect();
    out.sort_by(|a, b| a.token.cmp(&b.token));
    out.dedup_by(|a, b| a.token == b.token);
    out
}

/// What the proxy serves for the assignments `held`, each workload reached at its address in
/// `addresses`, with `certificates`: one route for each of their `ingress` endpoints. A
/// workload with no address yet has no route until its network is made. A residual assignment
/// runs nothing, so routes nothing.
#[must_use]
pub fn config(
    held: &HeldSet,
    addresses: &Addresses,
    certificates: Vec<ingress::Certificate>,
) -> ingress::IngressConfig {
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
            certificate: e.certificate.clone(),
            proxy_protocol: e.client_address.as_known()
                == Some(pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL),
            ..Default::default()
        }));
    }
    ingress::IngressConfig { routes, certificates, ..Default::default() }
}

#[cfg(test)]
mod tests;
