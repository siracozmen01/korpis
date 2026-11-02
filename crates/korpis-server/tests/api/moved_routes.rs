// SPDX-License-Identifier: AGPL-3.0-or-later

//! What follows a moved workload besides its lease: an address of a cluster pool it holds is
//! routed to the node it moved to, once the placement moved, by the router the pool names.

use crate::address_routes::{Fabric, arena, cluster, serving, shown};
use crate::migrating::{cut_over, moving_from, through_verify};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;

#[tokio::test]
async fn a_cluster_pool_s_address_is_routed_to_the_node_its_workload_moved_to() {
    let h = Harness::new().await;
    let fabric = Fabric::default();
    let _router = serving(&h, fabric.clone()).await;
    h.pools(h.root()).create_address_pool(cluster("p1", &h.root_org())).await.unwrap();
    let mut m = Box::pin(moving_from(h, arena, None)).await;
    let held = shown(&m.h, &m.db, |d| !d.waiting_on_router && !d.address.is_empty()).await;

    through_verify(&mut m, ("sha256:same", "sha256:same"), 4096).await;
    cut_over(&mut m).await;
    let get = pb::ListMigrationsRequest { workload: m.db.clone(), ..Default::default() };
    let listed = m.h.workloads(m.h.root()).list_migrations(get).await.unwrap().into_owned();
    let destination = listed.migrations.last().unwrap().destination.clone();
    fabric.last_asked_for(&(held.address, destination)).await;
}
