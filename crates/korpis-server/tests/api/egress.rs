// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress default's one exception (§6 of docs/07-networking.md): the control plane marks the
//! assignment of an installed extension's workload, and no other, as reaching its API; the node
//! opens the API's address to it and nothing else of the control plane's. And what a
//! declaration may ask of the network: what no node enforces is refused (Rule K-3).

use crate::providers::installed;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail};
use buffa::Message as _;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;
use std::time::Duration;

/// Whether the latest assignment of the workload with wire id `workload` says it reaches the
/// API, once it has one.
async fn reaches_api(h: &Harness, workload: &str) -> bool {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    let store = h.db.store().await.unwrap();
    for _ in 0..100 {
        let body: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT body FROM assignment WHERE workload_id = $1 ORDER BY generation DESC LIMIT 1",
        )
        .bind(id.uuid())
        .fetch_optional(store.pool())
        .await
        .unwrap();
        if let Some(b) = body {
            return agent::Assignment::decode_from_slice(&b).unwrap().reaches_api;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} was never assigned");
}

#[tokio::test]
async fn an_installed_extension_s_workload_reaches_the_api_and_no_other_does() {
    let h = Harness::new().await;
    let x = installed(&h, 41, "dns").await;
    assert!(reaches_api(&h, &x.extension.workload).await, "the extension's workload");

    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declare = create("d1", &project, "mc", body(&repo, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let plain = out.workload.as_option().unwrap().id.clone();
    assert!(!reaches_api(&h, &plain).await, "a workload that is no extension");
}

#[tokio::test]
async fn a_declaration_asking_what_no_node_enforces_is_refused_saying_why() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let endpoint = |exposure: pb::Exposure| pb::EndpointSpec {
        name: "game".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP.into(),
        container_port: 25565,
        exposure: exposure.into(),
        ..Default::default()
    };
    let refused = |b: pb::IntentBody, key: &str| {
        let declare = create(key, &project, key, b);
        let workloads = h.workloads(h.root());
        async move { workloads.declare_intent(declare).await.unwrap_err() }
    };

    let mut ingress = body(&repo, &[]);
    ingress.endpoints = vec![endpoint(pb::Exposure::EXPOSURE_INGRESS)];
    let e = refused(ingress, "ingress").await;
    assert_eq!(e.code, ErrorCode::Unimplemented);
    assert!(e.message.as_deref().unwrap_or_default().contains("Phase 1"), "{e:?}");

    let mut outbound = body(&repo, &[]);
    let mut game = endpoint(pb::Exposure::EXPOSURE_NODE_BOUND);
    game.policy = pb::NetworkPolicy {
        egress: vec![pb::NetworkRule {
            action: pb::NetworkAction::NETWORK_ACTION_DENY.into(),
            peer: Some(Peer::Cidr("203.0.113.0/24".into())),
            ..Default::default()
        }],
        ..Default::default()
    }
    .into();
    outbound.endpoints = vec![game];
    let e = refused(outbound, "outbound").await;
    assert_eq!(detail(&e).field, "body.endpoints[0].policy.egress", "{e:?}");

    let mut raised = body(&repo, &[]);
    raised.egress = pb::EgressPolicy {
        limits: pb::ConntrackLimits { max_connections: 1 << 20, ..Default::default() }.into(),
        ..Default::default()
    }
    .into();
    let e = refused(raised, "raised").await;
    assert_eq!(detail(&e).field, "body.egress.limits", "{e:?}");
}
