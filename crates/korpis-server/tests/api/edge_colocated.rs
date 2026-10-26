// SPDX-License-Identifier: AGPL-3.0-or-later

//! An edge group's probe on a control plane sharing its network with the node it would answer
//! through (§3.1 of docs/07-networking.md, "What it does not do"): that node's answer would be
//! delivered to the control plane locally and never cross the edge's link back, so once a replica
//! has recorded that it shares the node's network, every member is told to forward the probe to
//! another node; and once the record lapses, to the first again.

use crate::across::running_at;
use crate::agent::TestAgent;
use crate::dependencies::declare;
use crate::edge_groups::{arena_body, asked, cordon, edge_pool, edges_once, made};
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::support::Harness;
use korpis_app::ports::{EdgeLeaseWrite as _, Store as _, Tx as _};
use korpis_proto::korpis::agent::v1 as agent;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The node `told`'s first group forwards its probe to.
fn probed_through(told: &agent::Edges) -> &str {
    told.groups.first().map_or("", |g| g.probe_node.as_str())
}

#[tokio::test]
async fn the_probe_answers_through_a_node_the_control_plane_does_not_share_a_network_with() {
    let h = Harness::new().await;
    let (e1, e2, host) = (
        TestAgent::enroll(&h, "edge-1", 101).await,
        TestAgent::enroll(&h, "edge-2", 102).await,
        TestAgent::enroll(&h, "host-a", 103).await,
    );
    let mut s1 = opened(&h, &e1, side(1, "10.48.0.0/22", &[2, 3])).await;
    let _s2 = opened(&h, &e2, side(2, "10.48.4.0/22", &[1, 3])).await;
    let mut sa = opened(&h, &host, side(3, "10.48.8.0/22", &[1, 2])).await;
    cordon(&h, &e1.node).await;
    cordon(&h, &e2.node).await;
    let root = h.root_org();
    let group = made(&h, asked("g1", &root, "edge", &[&e1.node, &e2.node])).await;
    let pool = edge_pool("p1", &root, "198.51.100.0/29", &group.id);
    h.pools(h.root()).create_address_pool(pool).await.unwrap();
    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", arena_body(&push_paper().await)).await;
    running_at(&mut sa, &arena.id, "10.48.8.2").await;
    edges_once(&mut s1, |t| probed_through(t) == host.node).await;

    let id = |wire: &str| korpis_core::id::Id::parse(korpis_core::id::IdKind::Node, wire).unwrap();
    let store = h.db.store().await.unwrap();
    let record = |until: SystemTime| {
        let store = store.clone();
        let node = id(&host.node).uuid();
        async move {
            let mut tx = store.begin().await.unwrap();
            tx.record_colocated(Uuid::from_u128(77), &[node], until).await.unwrap();
            tx.commit().await.unwrap();
        }
    };
    record(SystemTime::now() + Duration::from_secs(60)).await;
    edges_once(&mut s1, |t| probed_through(t) == e2.node).await;
    record(SystemTime::now()).await;
    edges_once(&mut s1, |t| probed_through(t) == host.node).await;
}
