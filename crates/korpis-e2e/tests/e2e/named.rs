// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rules naming workloads on a real node (§6 of docs/07-networking.md, trace 76 of
//! docs/23-walkthroughs.md): a database whose endpoint lets in the workloads labelled `role=web`
//! and then denies every IPv4 address, and two workloads that depend on it, a web application so
//! labelled and a nightly report labelled `role=batch`. Both are let out by their dependencies,
//! and the database's rule, resolved by the control plane to the web application's address and
//! held by the node, answers the one and drops the other. The node's data plane is in a network
//! namespace of the test's own, as the mesh's test has it; skipped without root and containerd.

use crate::dependencies::{address_of, running};
use crate::mesh::{Netns, fetched, net_of, node_in, served};
use crate::node::live;
use crate::recipes::push_server;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;
use std::net::Ipv4Addr;

fn rule(action: pb::NetworkAction, peer: Peer) -> pb::NetworkRule {
    pb::NetworkRule { action: action.into(), peer: Some(peer), ..Default::default() }
}

/// The database's endpoint: its web applications, and no one else.
fn database() -> pb::IntentBody {
    let web = pb::LabelSelector {
        match_labels: [("role".to_owned(), "web".to_owned())].into(),
        ..Default::default()
    };
    let policy = pb::NetworkPolicy {
        ingress: vec![
            rule(pb::NetworkAction::NETWORK_ACTION_ALLOW, Peer::Workloads(Box::new(web))),
            rule(pb::NetworkAction::NETWORK_ACTION_DENY, Peer::Cidr("0.0.0.0/0".into())),
        ],
        ..Default::default()
    };
    let http = pb::EndpointSpec {
        name: "http".into(),
        exposure: pb::Exposure::EXPOSURE_OVERLAY_ONLY.into(),
        policy: policy.into(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![http], ..Default::default() }
}

/// A workload depending on `db`, reached by no one.
fn dependent(db: &str) -> pb::IntentBody {
    let dependency = pb::Dependency {
        name: "db".into(),
        target: Some(Target::Workload(db.to_owned())),
        required: true,
        ..Default::default()
    };
    let http = pb::EndpointSpec {
        name: "http".into(),
        exposure: pb::Exposure::EXPOSURE_NONE.into(),
        ..Default::default()
    };
    pb::IntentBody { dependencies: vec![dependency], endpoints: vec![http], ..Default::default() }
}

#[tokio::test]
async fn an_endpoint_lets_in_the_workloads_its_rule_names_and_no_other_dependent() {
    if !live() {
        return;
    }
    // The node's data plane in a namespace of the test's own, which forwards between its
    // workloads as a node does: this machine's own firewall drops forwarded traffic, which
    // would hide what the rule decides.
    let ns = Netns::new(format!("korpis-named-{}", std::process::id()));
    assert!(ns.run(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]));
    let cp = crate::support::ControlPlane::start().await;
    let project = cp.project().await;
    let node = node_in(&cp, "named-1", &ns, Ipv4Addr::new(198, 18, 3, 1)).await;
    let agent = node.agent().await;
    let repo = push_server().await;
    let db = cp.declare_body(&project, "db", &repo, database()).await;
    node.guard(&db);
    let labelled = |role| [("role", role)];
    let web = cp.declare_labelled(&project, ("web", &labelled("web")), &repo, dependent(&db)).await;
    node.guard(&web);
    let batch = labelled("batch");
    let report = cp.declare_labelled(&project, ("report", &batch), &repo, dependent(&db)).await;
    node.guard(&report);
    running(&web).await;
    running(&report).await;

    let to = format!("{}:8080", address_of(&node, &db).expect("the database's address"));
    let answered = served(&net_of(&node, &web), &to).await;
    assert!(answered.contains("served"), "its rule names the web application: {answered:?}");
    let refused = fetched(&net_of(&node, &report), &to, None).await;
    assert!(refused.is_empty(), "and not the report, which its dependency let out: {refused:?}");
    for w in [&web, &report, &db] {
        cp.retire(w).await;
    }
    agent.stop().await;
}
