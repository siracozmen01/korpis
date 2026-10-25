// SPDX-License-Identifier: AGPL-3.0-or-later

//! Enrolling a node (§3 of docs/18-operations.md, §7 of docs/02-architecture.md): a one-time
//! token bound to an organization, a name, and labels, redeemed online by the agent with the
//! public half of a key it generated itself.

use crate::support::{Harness, mutation, org_scope, reason};
use connectrpc::client::HttpClient;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

fn agents(h: &Harness) -> agent::AgentServiceClient<HttpClient> {
    agent::AgentServiceClient::new(h.http.clone(), h.config(None))
}

fn public(seed: u8) -> Vec<u8> {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key().to_bytes().to_vec()
}

fn token_request(key: &str, org: &str, name: &str) -> pb::CreateEnrollmentTokenRequest {
    pb::CreateEnrollmentTokenRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        node_name: name.to_owned(),
        labels: [("region".to_owned(), "fra".to_owned())].into_iter().collect(),
        ..Default::default()
    }
}

async fn token(h: &Harness, key: &str, name: &str) -> pb::CreateEnrollmentTokenResponse {
    let request = token_request(key, &h.root_org(), name);
    h.nodes(h.root()).create_enrollment_token(request).await.unwrap().into_owned()
}

fn enroll(token: &str, key: Vec<u8>) -> agent::EnrollRequest {
    agent::EnrollRequest {
        enrollment_token: token.to_owned(),
        node_public_key: key,
        hostname: "fra-3.example".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_token_enrolls_the_node_it_describes_once() {
    let h = Harness::new().await;
    let created = token(&h, "t1", "fra-3").await;
    assert!(created.token.starts_with("korpis_"));
    assert!(created.ca_fingerprint.starts_with("sha256:"), "the join command pins the CA");
    let replayed = token(&h, "t1", "fra-3").await;
    assert!(replayed.token.is_empty(), "a replay does not return the secret: it is not stored");

    let enrolled = agents(&h).enroll(enroll(&created.token, public(1))).await.unwrap();
    let enrolled = enrolled.into_owned();
    assert!(enrolled.assignment_keys.iter().any(|k| k.public_key.len() == 32));
    assert!(!enrolled.token_keys.is_empty());
    let node = h
        .nodes(h.root())
        .get_node(pb::GetNodeRequest { id: enrolled.node.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let node = node.node.as_option().unwrap();
    assert_eq!((node.name.as_str(), node.organization.as_str()), ("fra-3", h.root_org().as_str()));
    assert_eq!(node.labels.get("region").map(String::as_str), Some("fra"), "the token's labels");

    let again = agents(&h).enroll(enroll(&created.token, public(1))).await.unwrap();
    assert_eq!(again.into_owned().node, enrolled.node, "a lost answer can be asked for again");
    let other = agents(&h).enroll(enroll(&created.token, public(2))).await.unwrap_err();
    assert_eq!(reason(&other), "enrollment.used");
    let effects = h.effects("node.enroll").await;
    assert_eq!(effects.len(), 1, "enrolled once");
    assert!(effects[0].1.is_some(), "under the token's grant");
}

#[tokio::test]
async fn a_token_is_refused_once_expired_and_a_taken_name_is_refused() {
    let h = Harness::new().await;
    let mut request = token_request("t1", &h.root_org(), "fra-3");
    request.expires_in =
        buffa_types::google::protobuf::Duration { seconds: 1, ..Default::default() }.into();
    let short = h.nodes(h.root()).create_enrollment_token(request).await.unwrap().into_owned();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let e = agents(&h).enroll(enroll(&short.token, public(1))).await.unwrap_err();
    assert_eq!(reason(&e), "credential.missing_or_invalid");

    let first = token(&h, "t2", "fra-4").await;
    agents(&h).enroll(enroll(&first.token, public(2))).await.unwrap();
    let second = token(&h, "t3", "fra-4").await;
    let e = agents(&h).enroll(enroll(&second.token, public(3))).await.unwrap_err();
    assert_eq!(reason(&e), "node.name_taken");
    let bad = agents(&h).enroll(enroll(&second.token, vec![1, 2, 3])).await.unwrap_err();
    assert_eq!(reason(&bad), "request.invalid");
    let mut long = token_request("t4", &h.root_org(), "fra-5");
    long.expires_in =
        buffa_types::google::protobuf::Duration { seconds: 90_000, ..Default::default() }.into();
    let e = h.nodes(h.root()).create_enrollment_token(long).await.unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "a bearer credential for a node is short-lived");
}

#[tokio::test]
async fn a_use_limited_grant_is_spent_by_each_use() {
    let h = Harness::new().await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("once").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["org.read".into()],
            scope: org_scope(&h.root_org()).into(),
            conditions: pb::Conditions { max_uses: Some(1), ..Default::default() }.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let orgs = h.orgs(h.config(Some(secret.expose())));
    let get = || pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() };
    orgs.get_organization(get()).await.unwrap();
    let e = orgs.get_organization(get()).await.unwrap_err();
    assert_eq!(reason(&e), "object.not_found", "the one use is spent");
}

#[tokio::test]
async fn each_node_is_given_a_pool_of_its_own_and_a_removed_nodes_is_given_again() {
    let h = Harness::new().await;
    let mut pools = Vec::new();
    let mut nodes = Vec::new();
    for (i, name) in ["fra-1", "fra-2"].into_iter().enumerate() {
        let created = token(&h, &format!("p{i}"), name).await;
        let seed = u8::try_from(i).unwrap() + 1;
        let enrolled = agents(&h).enroll(enroll(&created.token, public(seed))).await.unwrap();
        let enrolled = enrolled.into_owned();
        let again = agents(&h).enroll(enroll(&created.token, public(seed))).await.unwrap();
        assert_eq!(again.into_owned().workload_ipv4, enrolled.workload_ipv4, "asked again");
        pools.push(enrolled.workload_ipv4);
        nodes.push(enrolled.node);
    }
    assert_eq!(pools, ["10.48.0.0/22", "10.48.4.0/22"], "the lowest free /22 of the range");

    let admin = h.nodes(h.root());
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c").into(),
        node: nodes[0].clone(),
        ..Default::default()
    };
    admin.cordon_node(cordon).await.unwrap();
    let remove = pb::RemoveNodeRequest {
        mutation: mutation("r").into(),
        node: nodes[0].clone(),
        ..Default::default()
    };
    admin.remove_node(remove).await.unwrap();
    let created = token(&h, "p3", "fra-3").await;
    let third = agents(&h).enroll(enroll(&created.token, public(3))).await.unwrap();
    assert_eq!(third.into_owned().workload_ipv4, "10.48.0.0/22", "freed by the removal");
}
