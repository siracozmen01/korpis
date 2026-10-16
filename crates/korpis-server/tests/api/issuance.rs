// SPDX-License-Identifier: AGPL-3.0-or-later

//! The issuer of `acme` certificates against Pebble (§3.2 of docs/07-networking.md), every
//! challenge taken as valid: an endpoint left to its default is given a certificate when its
//! routes are claimed; its order's challenge reaches the node it is placed on in its assignment,
//! and the directory is told to look only once that node has acknowledged it; what is issued
//! reaches the node as the endpoint's certificate at a new revision, the challenge gone, and the
//! node is given it. Without the directory's terms agreed, the certificate fails and says why.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{assignment, until};
use crate::recipes::{project, push_paper};
use crate::routes::{attest, declare, web};
use crate::support::Harness;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use korpis_server::acme::AcmeConfig;
use std::time::{Duration, SystemTime};

/// A node serving the ingress, enrolled and connected, and a workload with an `acme` endpoint
/// for `shop.example.com` placed on it.
async fn placed(h: &Harness) -> Stream {
    let edge = TestAgent::enroll(h, "fra-1", 1).await;
    let (s, first) = TestAgent::open(h, edge.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true").execute(store.pool()).await.unwrap();
    let project = project(h, &h.root_org(), "shops").await;
    attest(h, "d1", &h.root_org(), "example.com").await;
    declare(h, &project, "shop", web(&push_paper().await, &["shop.example.com"])).await;
    s
}

/// The digest of the first set whose assignment carries a challenge, and the challenge.
fn challenged(d: &Down) -> Option<(String, agent::AcmeChallenge)> {
    let Down::AssignmentSet(s) = d else { return None };
    let a = agent::Assignment::decode_from_slice(&s.assignments.first()?.assignment).ok()?;
    Some((s.set_digest.clone(), a.challenges.first()?.clone()))
}

async fn send(s: &mut Stream, up: Up) {
    s.send(agent::AgentMessage { message: Some(up), ..Default::default() }).await.unwrap();
}

#[tokio::test]
async fn an_endpoints_certificate_is_ordered_once_its_node_holds_the_challenge() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let h = Harness::with_acme(pebble.config(), None).await;
    let mut s = placed(&h).await;

    let (digest, challenge) = until(&mut s, challenged).await;
    assert!(challenge.key_authorization.starts_with(&format!("{}.", challenge.token)));
    // Nothing is issued while the node has not acknowledged the set carrying the challenge.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let listed = pb::ListCertificatesRequest { organization: h.root_org(), ..Default::default() };
    let certs = h.certificates(h.root()).list_certificates(listed.clone()).await.unwrap();
    let pending = certs.into_owned().certificates;
    assert_eq!(pending[0].state.as_known(), Some(pb::CertificateState::CERTIFICATE_STATE_PENDING));
    send(&mut s, Up::Ack(Box::new(agent::Ack { set_digest: digest, ..Default::default() }))).await;

    let served = until(
        &mut s,
        assignment(|a| {
            a.challenges.is_empty() && a.endpoints.iter().any(|e| e.certificate_revision == 1)
        }),
    )
    .await;
    let id = served.endpoints.iter().find(|e| e.name == "web").unwrap().certificate.clone();
    let request = agent::CertificateRequest {
        request: "r1".into(),
        certificate: id.clone(),
        ..Default::default()
    };
    send(&mut s, Up::CertificateRequest(Box::new(request))).await;
    let pick = |d: &Down| match d {
        Down::CertificateKey(k) if k.request == "r1" => k.outcome.clone(),
        _ => None,
    };
    let Outcome::Certificate(c) = until(&mut s, pick).await else { panic!("refused") };
    assert!(c.chain.contains("BEGIN CERTIFICATE") && c.key.contains("PRIVATE KEY"));
    let certs = h.certificates(h.root()).list_certificates(listed).await.unwrap().into_owned();
    let issued = certs.certificates.iter().find(|c| c.id == id).unwrap();
    assert_eq!(issued.names, ["shop.example.com"]);
    assert_eq!(issued.source.as_known(), Some(pb::CertificateSource::CERTIFICATE_SOURCE_ACME));
    assert!(issued.renew_after.as_option().is_some(), "renewed two thirds through");
}

#[tokio::test]
async fn without_the_directorys_terms_agreed_a_certificate_fails_saying_why() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let h = Harness::with_acme(AcmeConfig { agreed: false, ..pebble.config() }, None).await;
    let _s = placed(&h).await;
    let listed = pb::ListCertificatesRequest { organization: h.root_org(), ..Default::default() };
    for _ in 0..300 {
        let certs = h.certificates(h.root()).list_certificates(listed.clone()).await.unwrap();
        let certs = certs.into_owned().certificates;
        if let Some(c) = certs.first().filter(|c| !c.reason.is_empty()) {
            assert_eq!(c.state.as_known(), Some(pb::CertificateState::CERTIFICATE_STATE_FAILED));
            assert!(c.reason.contains("--acme-agree-terms"), "{}", c.reason);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the certificate never said why it was not issued");
}
