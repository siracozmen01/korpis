// SPDX-License-Identifier: AGPL-3.0-or-later

//! A `stable` endpoint no edge group serves, routed at a `cluster` pool's address (§3.1 of
//! docs/07-networking.md, "Where a workload may run"): its workload holds the pool's address as
//! a `dedicated_ip` workload does, wherever it is placed, the pool's router is asked to route it
//! to the node, and the endpoint is reached there on its own port, holding no port of its node;
//! a plan says the endpoint resolves to `stable`.

use crate::address_routes::{Fabric, cluster, serving, shown};
use crate::dependencies::{declare, placement};
use crate::egress::assigned;
use crate::recipes::{body, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;

fn game(repo: &str) -> pb::IntentBody {
    let game = pb::EndpointSpec {
        name: "game".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP.into(),
        container_port: 25_565,
        exposure: pb::Exposure::EXPOSURE_STABLE.into(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![game], ..body(repo, &[]) }
}

#[tokio::test]
async fn a_stable_endpoint_no_group_serves_is_routed_at_a_cluster_pool_s_address() {
    let h = Harness::new().await;
    let root = h.root_org();
    let fabric = Fabric::default();
    let _router = serving(&h, fabric.clone()).await;
    h.pools(h.root()).create_address_pool(cluster("p1", &root)).await.unwrap();

    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", game(&push_paper().await)).await;
    let node = placement(&h, &arena.id).await.node;
    let routed = shown(&h, &arena.id, |d| !d.waiting_on_router && !d.address.is_empty()).await;
    assert_eq!(routed.address, "203.0.113.2", "the cluster pool's address");
    assert!(fabric.routes().contains(&("203.0.113.2".to_owned(), node)), "routed to its node");

    let get = pb::GetWorkloadRequest { id: arena.id.clone(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let status = got.status.into_option().unwrap();
    let endpoint = &status.endpoints[0];
    assert_eq!(endpoint.exposure.as_known(), Some(pb::Exposure::EXPOSURE_STABLE), "resolved so");
    let at = endpoint.address.as_option().expect("reached at the address");
    assert_eq!((at.ips.as_slice(), at.port), (&["203.0.113.2".to_owned()][..], 25_565));

    let told = assigned(&h, &arena.id).await;
    assert_eq!(told.dedicated.as_option().map(|d| d.address.as_str()), Some("203.0.113.2"));
    let placed = &told.endpoints[0];
    assert!(placed.address.as_option().is_none(), "no port of its node: {placed:?}");
}
