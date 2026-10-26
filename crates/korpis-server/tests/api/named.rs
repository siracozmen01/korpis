// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rules naming workloads (§6 of docs/07-networking.md, trace 76 of docs/23-walkthroughs.md), the
//! control plane's half: a database's endpoint rule naming the workloads labelled `role=web` is
//! resolved, for the database's node, to the address the web application's node observed it at,
//! in that node's own pool, and is sent in the database's assignment; and the database's status
//! says what the rule matched and what it names at no address. A workload observed stopped is
//! named no longer.

use crate::across::{assigned_once, observed_at, running_at};
use crate::agent::TestAgent;
use crate::dependencies::{declare, one_endpoint, placement};
use crate::mesh::{opened, side};
use crate::recipes::{create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;

/// The database: its `game` endpoint lets in the workloads labelled `role=web`.
fn database(repo: &str) -> pb::IntentBody {
    let web = pb::LabelSelector {
        match_labels: [("role".to_owned(), "web".to_owned())].into(),
        ..Default::default()
    };
    let allow = pb::NetworkRule {
        action: pb::NetworkAction::NETWORK_ACTION_ALLOW.into(),
        peer: Some(Peer::Workloads(Box::new(web))),
        ..Default::default()
    };
    let game = pb::EndpointSpec {
        name: "game".into(),
        policy: pb::NetworkPolicy { ingress: vec![allow], ..Default::default() }.into(),
        ..Default::default()
    };
    let mut body = one_endpoint(repo);
    body.endpoints.push(game);
    body
}

#[tokio::test]
async fn a_rule_naming_workloads_names_the_addresses_its_node_routes_to_them() {
    let h = Harness::new().await;
    let (a, b) =
        (TestAgent::enroll(&h, "named-a", 61).await, TestAgent::enroll(&h, "named-b", 62).await);
    let sa = opened(&h, &a, side(1, "10.48.0.0/22", &[2])).await;
    let sb = opened(&h, &b, side(2, "10.48.4.0/22", &[1])).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shop").await;
    let db = declare(&h, &project, "db", database(&repo)).await;
    let db_node = placement(&h, &db.id).await.node;
    let (mut other, address) =
        if db_node == a.node { (sb, "10.48.4.2") } else { (sa, "10.48.0.2") };
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("cordon-db").into(),
        node: db_node.clone(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(cordon).await.unwrap();

    let mut d = create("web", &project, "web", one_endpoint(&repo));
    if let Some(Target::Create(new)) = d.target.as_mut() {
        new.labels = [("role".to_owned(), "web".to_owned())].into();
    }
    let web = h.workloads(h.root()).declare_intent(d).await.unwrap().into_owned();
    let web = web.workload.into_option().unwrap();
    assert_ne!(placement(&h, &web.id).await.node, db_node, "on the other node of the mesh");
    running_at(&mut other, &web.id, address).await;

    let told = assigned_once(&h, &db.id, |a| a.peers.iter().any(|p| !p.addresses.is_empty())).await;
    let peers = &told.peers[0];
    assert_eq!((peers.endpoint.as_str(), peers.rule), ("game", 0));
    assert_eq!(peers.addresses, [address], "the web application, in its node's pool");

    let get = pb::GetWorkloadRequest { id: db.id.clone(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let shown = got.status.into_option().unwrap().named_rules;
    assert_eq!(shown[0].matched, std::slice::from_ref(&web.id));
    assert!(shown[0].left_out.is_empty(), "{:?}", shown[0].left_out);

    // Stopped, it is no longer named: its node may give its address to another workload
    // (Finding 184 of docs/23-walkthroughs.md).
    let stopped = pb::ObservedState::OBSERVED_STATE_STOPPED;
    observed_at(&mut other, (&web.id, address), stopped).await;
    let told = assigned_once(&h, &db.id, |a| a.peers.iter().all(|p| p.addresses.is_empty())).await;
    assert_eq!(told.peers.len(), 1, "the rule is still told, naming no one");
}
