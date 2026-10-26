// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placing a workload's endpoints on its node as its assignment is computed (§3 of
//! docs/07-networking.md). The control plane holds every placement on the node and the range
//! of ports it advertises, so it alone can keep two endpoints from one port: each endpoint the
//! node itself exposes keeps the port it holds there, whatever the node's range has become
//! since, and one that holds none is given the lowest free port of the range as it is now.

use crate::error::AppError;
use crate::ports::Tx;
use korpis_core::node::advertised::PortRange;
use korpis_core::workload::IntentBody;
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::spec::endpoints::{Exposure, TlsSpec};
use std::time::SystemTime;
use uuid::Uuid;

/// `body`'s endpoints placed on `node` for `workload`, sorted by name, the node advertising the
/// ports in `range`, an edge serving those named in `edged` (§3.1 of docs/07-networking.md),
/// which hold no port of the node. An endpoint that needs a port when none is free is placed without one,
/// and its status says it is not reached; placement keeps this from happening by reserving
/// the node's ports (§2.1 of docs/05-scheduling.md), so only a range narrowed under it does.
pub(super) async fn placed<T: Tx>(
    tx: &mut T,
    (node, workload): (Uuid, Uuid),
    (body, edged): (&IntentBody, &[String]),
    range: Option<PortRange>,
    at: SystemTime,
) -> Result<Vec<Endpoint>, AppError> {
    let names: Vec<String> = body.endpoints.iter().map(|e| e.name.clone()).collect();
    tx.release_endpoints(node, workload, &names).await?;
    let held = tx.held_endpoints(workload).await?;
    let served = tx.served_for(workload).await?;
    let mut out = Vec::with_capacity(body.endpoints.len());
    for declared in &body.endpoints {
        let before = held.iter().find(|e| e.name == declared.name);
        let exposure = declared.exposure.resolved(edged.contains(&declared.name));
        let mut port = before.and_then(|e| e.port).filter(|_| exposure.holds_node_port());
        if exposure.holds_node_port() && port.is_none() {
            port = match range {
                Some(r) => tx.free_node_port(node, r).await?,
                None => None,
            };
        }
        let ingress = exposure == Exposure::Ingress;
        let endpoint = Endpoint {
            id: before.map_or_else(Uuid::now_v7, |e| e.id),
            workload,
            name: declared.name.clone(),
            protocol: declared.protocol,
            container_port: declared.container_port,
            exposure,
            node,
            port,
            domains: if ingress { declared.domains.clone() } else { Vec::new() },
            tls: ingress.then(|| declared.tls.clone().unwrap_or(TlsSpec::Acme { clients: None })),
            certificate: served.iter().find(|(e, _)| *e == declared.name).map(|(_, c)| *c),
        };
        if before != Some(&endpoint) {
            tx.put_endpoint(&endpoint, at).await?;
        }
        out.push(endpoint);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
