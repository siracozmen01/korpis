// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a member's proxy serves for its edge groups (§3.1 of docs/07-networking.md): every
//! `ingress` endpoint its groups serve, routed to its workload's address, through the mesh where
//! it runs on another node, with the certificates those name and the challenges standing for
//! their names. A route the node's own assignments make already is not made twice.

use super::route;
use crate::held::HeldSet;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::ingress::v1 as ingress;
use std::collections::BTreeMap;

/// The routes `edges` names, but those of a workload the node holds an assignment for, which its
/// own routes serve.
fn routes<'a>(
    edges: &'a agent::Edges,
    held: &'a HeldSet,
) -> impl Iterator<Item = (&'a agent::EdgeProxied, &'a korpis_proto::korpis::v1::Endpoint)> {
    edges
        .groups
        .iter()
        .flat_map(|g| &g.proxied)
        .filter(|p| !p.workload_address.is_empty())
        .filter_map(|p| p.endpoint.as_option().map(|e| (p, e)))
        .filter(|(_, e)| held.assignments.get(&e.workload).is_none_or(|a| a.residual))
}

/// `config` with what `edges` asks of the proxy beside it: each route its groups serve, which
/// wants its certificate at its revision, added to `wanted`; and the challenges standing for
/// their names.
#[must_use]
pub fn with_edges(
    mut config: ingress::IngressConfig,
    (edges, held): (&agent::Edges, &HeldSet),
    wanted: &mut BTreeMap<String, i64>,
) -> ingress::IngressConfig {
    let mut seen: Vec<(String, String)> = Vec::new();
    for (p, e) in routes(edges, held) {
        let key = (e.workload.clone(), e.name.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if !e.certificate.is_empty() {
            let at = wanted.entry(e.certificate.clone()).or_insert(e.certificate_revision);
            *at = (*at).max(e.certificate_revision);
        }
        config.routes.push(route(&e.workload, e, &p.workload_address));
    }
    config.challenges.extend(edges.challenges.iter().map(|c| ingress::Challenge {
        token: c.token.clone(),
        key_authorization: c.key_authorization.clone(),
        ..Default::default()
    }));
    config.alpn_challenges.extend(edges.alpn_challenges.iter().map(|c| ingress::AlpnChallenge {
        name: c.name.to_ascii_lowercase(),
        digest: c.digest.clone(),
        ..Default::default()
    }));
    config
}

/// What the node says its proxy holds of `edges`' challenges, once it has been given them.
#[must_use]
pub fn held_proofs(edges: &agent::Edges) -> agent::EdgeChallengesHeld {
    agent::EdgeChallengesHeld {
        tokens: edges.challenges.iter().map(|c| c.token.clone()).collect(),
        alpn_digests: edges.alpn_challenges.iter().map(|c| c.digest.clone()).collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    fn proxied(workload: &str, at: &str) -> agent::EdgeProxied {
        agent::EdgeProxied {
            endpoint: pb::Endpoint {
                workload: workload.into(),
                name: "web".into(),
                container_port: 8080,
                exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
                domains: vec![format!("{workload}.example.com")],
                certificate: format!("crt_{workload}"),
                certificate_revision: 3,
                ..Default::default()
            }
            .into(),
            node: "nd_a".into(),
            workload_address: at.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_member_routes_its_groups_names_to_their_workloads_and_answers_their_challenges() {
        let edges = agent::Edges {
            groups: vec![agent::EdgeForwarding {
                proxied: vec![
                    proxied("w1", "10.48.4.2"),
                    proxied("w2", ""),
                    proxied("w3", "10.48.0.5"),
                ],
                ..Default::default()
            }],
            challenges: vec![agent::AcmeChallenge {
                token: "t".into(),
                key_authorization: "t.k".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut held = HeldSet::default();
        held.assignments.insert("w3".into(), agent::Assignment::default());
        let mut wanted = BTreeMap::new();
        let config = with_edges(ingress::IngressConfig::default(), (&edges, &held), &mut wanted);
        let routes: Vec<(&str, &str)> =
            config.routes.iter().map(|r| (r.names[0].as_str(), r.backend.as_str())).collect();
        assert_eq!(routes, [("w1.example.com", "10.48.4.2:8080")], "not running; its own route");
        assert_eq!(wanted, BTreeMap::from([("crt_w1".to_owned(), 3)]));
        assert_eq!(config.challenges[0].token, "t");
        assert_eq!(held_proofs(&edges).tokens, ["t"]);
    }
}
