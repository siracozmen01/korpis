// SPDX-License-Identifier: AGPL-3.0-or-later

//! A bandwidth reservation placed (§5.2 of docs/05-scheduling.md, §7 of docs/07-networking.md):
//! only on a node that offers bandwidth, which a node with a shaped uplink does, held there as
//! memory is held, and never beyond what the node offers.

use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_core::tenancy::quota::Dimension;
use korpis_proto::korpis::v1 as pb;

const MBIT: u64 = 1_000_000;

async fn declared(h: &Harness, project: &str, repo: &str, (name, reserved): (&str, u64)) -> String {
    let mut b = body(repo, &[]);
    let bandwidth = pb::BandwidthSpec {
        egress: pb::ReservedLimit {
            reservation: reserved,
            limit: reserved.max(400 * MBIT),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    b.resources = pb::ResourceSpec { bandwidth: bandwidth.into(), ..Default::default() }.into();
    let declare = create(&format!("d-{name}"), project, name, b);
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    out.workload.as_option().unwrap().id.clone()
}

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

#[tokio::test]
async fn a_reservation_lands_on_the_node_with_a_shaped_uplink_and_is_never_oversubscribed() {
    let h = Harness::new().await;
    let org = h.installed.organization.id;
    // A node with no uplink declared offers no bandwidth and enforces no egress.
    let mut plain = korpis_store::testing::ready_node(org, "fra-1", &[]).unwrap();
    plain.capacity.remove(&Dimension::Bandwidth);
    plain.allocatable.remove(&Dimension::Bandwidth);
    plain.drivers[0].capabilities.enforcement.remove("egress");
    h.enroll(plain).await;
    let mut shaped = korpis_store::testing::ready_node(org, "fra-2", &[]).unwrap();
    shaped.capacity.insert(Dimension::Bandwidth, 1_000 * MBIT);
    shaped.allocatable.insert(Dimension::Bandwidth, 950 * MBIT);
    let uplinked = h.enroll(shaped).await;
    let (repo, project) = (push_paper().await, project(&h, &h.root_org(), "games").await);

    let mc = declared(&h, &project, &repo, ("mc", 100 * MBIT)).await;
    let s = status(&h, &mc).await;
    let placement = s.placement.as_option().expect("placed");
    assert_eq!(placement.node, uplinked, "the node that offers bandwidth");
    assert_eq!(placement.reserved.bandwidth_bits_per_sec, 100 * MBIT, "held as memory is");

    let big = declared(&h, &project, &repo, ("big", 900 * MBIT)).await;
    let s = status(&h, &big).await;
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert!(why.filtered.iter().any(|f| f.check == "capacity" && f.detail.contains("bandwidth")));
}
