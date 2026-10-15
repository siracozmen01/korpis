// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3 of docs/07-networking.md): when a node's assignment is computed,
//! each endpoint its workloads declare is placed there with the exposure it resolved to, and one
//! the node itself exposes is given a port of the node's range that no other endpoint holds.
//! The node is told the port, a client is shown the address, a node with no free port is not
//! chosen, and the probe never listens on a port an endpoint holds (Finding 53).

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{assignment, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

async fn session(h: &Harness, name: &str, seed: u8) -> (TestAgent, Stream) {
    let a = TestAgent::enroll(h, name, seed).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[seed; 16])).await;
    welcome(&first.unwrap());
    (a, s)
}

/// Declares the example as `name` in `project`; its workload's id.
async fn declared(h: &Harness, project: &str, repo: &str, name: &str) -> String {
    let declare = create(&format!("d-{name}"), project, name, body(repo, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    out.workload.as_option().unwrap().id.clone()
}

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

/// `w`'s endpoints, once its node's assignment placed them.
async fn placed(h: &Harness, w: &str) -> Vec<pb::Endpoint> {
    for _ in 0..100 {
        let s = status(h, w).await;
        if !s.endpoints.is_empty() {
            return s.endpoints;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} endpoints never placed: {:?}", status(h, w).await.unplaced);
}

fn port_of(endpoints: &[pb::Endpoint], name: &str) -> Option<u32> {
    let e = endpoints.iter().find(|e| e.name == name)?;
    e.address.as_option().map(|a| a.port)
}

#[tokio::test]
async fn each_endpoint_the_node_exposes_holds_a_port_of_its_own_and_the_node_is_told_it() {
    let h = Harness::new().await;
    let (a, mut s) = session(&h, "fra-1", 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let first = declared(&h, &project, &repo, "mc").await;
    let endpoints = placed(&h, &first).await;

    let game = endpoints.iter().find(|e| e.name == "game").unwrap();
    assert_eq!(game.exposure.as_known(), Some(pb::Exposure::EXPOSURE_NODE_BOUND), "stable");
    let address = game.address.as_option().unwrap();
    assert!((30000..=32767).contains(&address.port), "{}", address.port);
    assert_eq!(address.ips, ["203.0.113.10"], "where a client reaches it");
    assert_eq!(address.node, a.node);
    assert_eq!(game.client_address.as_known(), Some(pb::ClientAddress::CLIENT_ADDRESS_PRESERVED));
    let rcon = endpoints.iter().find(|e| e.name == "rcon").unwrap();
    assert_eq!(rcon.exposure.as_known(), Some(pb::Exposure::EXPOSURE_OVERLAY_ONLY));
    assert!(!rcon.address.is_set(), "the node does not expose it");

    let told = until(&mut s, assignment(|a| !a.endpoints.is_empty())).await;
    assert_eq!(port_of(&told.endpoints, "game"), Some(address.port), "the node forwards it");

    let second = declared(&h, &project, &repo, "mc-2").await;
    let other = port_of(&placed(&h, &second).await, "game").unwrap();
    assert_ne!(other, address.port, "one port, one endpoint");
    assert_eq!(placed(&h, &first).await, endpoints, "and the first keeps its own");
}

#[tokio::test]
async fn a_node_whose_ports_are_all_held_is_not_chosen() {
    let h = Harness::new().await;
    let (a, _s) = session(&h, "fra-1", 1).await;
    // Three ports: one for each endpoint the example's node exposes, `game` and `query`, and
    // one kept for the probe.
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Node, &a.node).unwrap();
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET first_port = 30000, last_port = 30002 WHERE id = $1")
        .bind(id.uuid())
        .execute(store.pool())
        .await
        .unwrap();
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let first = declared(&h, &project, &repo, "mc").await;
    let endpoints = placed(&h, &first).await;
    let mut ports = [port_of(&endpoints, "game"), port_of(&endpoints, "query")];
    ports.sort_unstable();
    assert_eq!(ports, [Some(30000), Some(30001)]);

    let second = declared(&h, &project, &repo, "mc-2").await;
    let s = status(&h, &second).await;
    assert!(!s.placement.is_set(), "{:?}", s.placement);
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    let refused = why.filtered.iter().map(|f| f.detail.clone()).collect::<Vec<_>>().join("; ");
    assert!(refused.contains("node_ports: needs 2, 0 free"), "{refused}");
    // What the probe is kept from: the port the endpoint holds.
    let held = korpis_app::ports::EndpointRead::node_ports_held(&store, id.uuid()).await.unwrap();
    assert_eq!(held, [30000, 30001]);
}

#[tokio::test]
async fn the_plan_that_creates_an_endpoint_says_what_it_resolves_to_and_survives() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let mut declare = create("d1", &project, "mc", body(&repo, &[]));
    declare.propose = true;
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let impact = out.plan.as_option().unwrap().impact.as_option().unwrap().clone();
    let shown = |name: &str| impact.exposures.iter().find(|e| e.endpoint == name).cloned().unwrap();
    let game = shown("game");
    assert_eq!(game.declared.as_known(), Some(pb::Exposure::EXPOSURE_STABLE));
    assert_eq!(game.resolved.as_known(), Some(pb::Exposure::EXPOSURE_NODE_BOUND), "no edge");
    assert_eq!(
        game.survives.as_known(),
        Some(pb::ExposureSurvival::EXPOSURE_SURVIVAL_WHILE_ITS_NODE_EXISTS)
    );
    assert!(game.measured_inside, "the operator owns the firewall in front of the node");
    let rcon = shown("rcon");
    assert_eq!(rcon.survives.as_known(), Some(pb::ExposureSurvival::EXPOSURE_SURVIVAL_MOVES));
    assert!(!rcon.measured_inside);
}
