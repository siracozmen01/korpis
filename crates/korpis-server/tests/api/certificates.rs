// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provided certificates (§3.2 of docs/07-networking.md): uploaded chain and key, the key shown
//! to be the leaf's and never answered; named by an endpoint only if they are its organization's,
//! valid, and cover every name it routes; replaced in place only by a chain still covering what
//! is served with them; and deleted only once no endpoint serves with them. A node is given one,
//! key and all, only if an endpoint served with it is placed there, and its assignment names the
//! certificate's revision, so a replacement reaches it.

use crate::agent::{Stream, TestAgent, welcome};
use crate::domains::child;
use crate::journal::{assignment, until};
use crate::recipes::{change, project, push_paper};
use crate::routes::{attest, declare, refused, web};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// A self-signed certificate for `names` and its key, in PEM.
fn issued(names: &[&str]) -> (String, String) {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let made = rcgen::generate_simple_self_signed(names).unwrap();
    (made.cert.pem(), made.signing_key.serialize_pem())
}

async fn upload(
    h: &Harness,
    key: &str,
    org: &str,
    (chain, pem): (String, String),
) -> pb::Certificate {
    let req = pb::CreateCertificateRequest {
        mutation: mutation(key).into(),
        organization: org.into(),
        chain,
        key: pem,
        ..Default::default()
    };
    let made = h.certificates(h.root()).create_certificate(req).await.unwrap().into_owned();
    made.certificate.into_option().unwrap()
}

/// `web` serving its names with the provided certificate `id`.
fn served(repo: &str, names: &[&str], id: &str) -> pb::IntentBody {
    let mut body = web(repo, names);
    body.endpoints[0].tls = pb::TlsSpec {
        source: pb::TlsSource::TLS_SOURCE_PROVIDED.into(),
        secret: id.into(),
        ..Default::default()
    }
    .into();
    body
}

#[tokio::test]
async fn an_upload_is_read_for_its_names_and_its_key_is_never_answered() {
    let h = Harness::new().await;
    let c = upload(&h, "c1", &h.root_org(), issued(&["shop.example.com", "*.example.com"])).await;
    assert!(c.id.starts_with("crt_"));
    assert_eq!(c.names, ["shop.example.com", "*.example.com"]);
    assert_eq!(c.state.as_known(), Some(pb::CertificateState::CERTIFICATE_STATE_ISSUED));
    assert_eq!(c.source.as_known(), Some(pb::CertificateSource::CERTIFICATE_SOURCE_PROVIDED));
    assert_eq!(c.revision, 1);
    assert!(c.chain.starts_with("-----BEGIN CERTIFICATE-----") && !c.chain.contains("PRIVATE"));

    let (chain, _) = issued(&["shop.example.com"]);
    let (_, other) = issued(&["shop.example.com"]);
    let mismatched = pb::CreateCertificateRequest {
        mutation: mutation("c2").into(),
        organization: h.root_org(),
        chain,
        key: other,
        ..Default::default()
    };
    let e = h.certificates(h.root()).create_certificate(mismatched).await.unwrap_err();
    assert!(e.to_string().contains("key"), "{e}");

    let listed = pb::ListCertificatesRequest { organization: h.root_org(), ..Default::default() };
    let listed = h.certificates(h.root()).list_certificates(listed).await.unwrap().into_owned();
    assert_eq!(listed.certificates.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), [c.id]);
}

#[tokio::test]
async fn an_endpoint_serves_with_a_certificate_of_its_own_covering_its_names() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;
    let narrow = upload(&h, "c1", &h.root_org(), issued(&["www.example.com"])).await;
    let beta = child(&h, "beta").await;
    let theirs = upload(&h, "c2", &beta, issued(&["*.example.com"])).await;
    let ours = upload(&h, "c3", &h.root_org(), issued(&["*.example.com"])).await;

    let names = ["shop.example.com", "www.example.com"];
    let uncovered = served(&repo, &names, &narrow.id);
    assert_eq!(refused(&h, &project, "a", uncovered).await, "endpoint.certificate");
    let foreign = served(&repo, &names, &theirs.id);
    assert_eq!(refused(&h, &project, "b", foreign).await, "endpoint.certificate");
    let malformed = served(&repo, &names, "not-an-id");
    assert_eq!(refused(&h, &project, "c", malformed).await, "request.invalid");
    let shop = declare(&h, &project, "shop", served(&repo, &names, &ours.id)).await;

    let delete = |key: &str, id: &str| pb::DeleteCertificateRequest {
        mutation: mutation(key).into(),
        id: id.to_owned(),
        ..Default::default()
    };
    let certs = h.certificates(h.root());
    let e = certs.delete_certificate(delete("x1", &ours.id)).await.unwrap_err();
    assert_eq!(reason(&e), "certificate.in_use");

    let replace = |key: &str, (chain, key_pem): (String, String)| pb::ReplaceCertificateRequest {
        mutation: mutation(key).into(),
        id: ours.id.clone(),
        chain,
        key: key_pem,
        ..Default::default()
    };
    let e = certs.replace_certificate(replace("r1", issued(&["shop.example.com"]))).await;
    assert_eq!(reason(&e.unwrap_err()), "certificate.uncovered", "www is served with it");
    let renewed = certs.replace_certificate(replace("r2", issued(&names))).await.unwrap();
    let renewed = renewed.into_owned().certificate.into_option().unwrap();
    assert_eq!((renewed.id.as_str(), renewed.revision), (ours.id.as_str(), 2));

    let plain = change("m1", &shop.id, shop.version, web(&repo, &names));
    h.workloads(h.root()).declare_intent(plain).await.unwrap();
    certs.delete_certificate(delete("x2", &ours.id)).await.expect("no endpoint serves with it");
}

/// Asks for `certificate` as the node of stream `s`; the outcome.
async fn fetch(s: &mut Stream, request: &str, certificate: &str) -> Outcome {
    let up = agent::CertificateRequest {
        request: request.into(),
        certificate: certificate.into(),
        ..Default::default()
    };
    let message = agent::AgentMessage {
        message: Some(Up::CertificateRequest(Box::new(up))),
        ..Default::default()
    };
    s.send(message).await.unwrap();
    let pick = |d: &Down| match d {
        Down::CertificateKey(k) if k.request == request => k.outcome.clone(),
        _ => None,
    };
    until(s, pick).await
}

#[tokio::test]
async fn the_key_goes_to_the_node_its_endpoint_is_placed_on_and_again_when_replaced() {
    let h = Harness::new().await;
    let edge = TestAgent::enroll(&h, "fra-1", 1).await;
    let other = TestAgent::enroll(&h, "fra-2", 2).await;
    let (mut s, first) = TestAgent::open(&h, edge.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let (mut elsewhere, first) =
        TestAgent::open(&h, other.hello_at(SystemTime::now(), &[2; 16])).await;
    welcome(&first.unwrap());
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = (name = 'fra-1')").execute(store.pool()).await.unwrap();
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;
    let (chain, key) = issued(&["shop.example.com"]);
    let c = upload(&h, "c1", &h.root_org(), (chain, key.clone())).await;
    declare(&h, &project, "shop", served(&repo, &["shop.example.com"], &c.id)).await;

    let carries = |revision: i64| {
        let id = c.id.clone();
        assignment(move |a| {
            a.endpoints.iter().any(|e| e.certificate == id && e.certificate_revision == revision)
        })
    };
    until(&mut s, carries(1)).await;
    match fetch(&mut s, "r1", &c.id).await {
        Outcome::Certificate(served) => {
            assert_eq!((served.revision, served.key.as_str()), (1, key.as_str()));
        }
        Outcome::Refused(e) => panic!("refused: {e:?}"),
    }
    match fetch(&mut elsewhere, "r2", &c.id).await {
        Outcome::Refused(e) => assert_eq!(e.reason, "certificate.not_here"),
        Outcome::Certificate(_) => panic!("a node it is not placed on was given the key"),
    }

    let (chain, key) = issued(&["shop.example.com"]);
    let replace = pb::ReplaceCertificateRequest {
        mutation: mutation("r1").into(),
        id: c.id.clone(),
        chain,
        key,
        ..Default::default()
    };
    h.certificates(h.root()).replace_certificate(replace).await.unwrap();
    until(&mut s, carries(2)).await;
}
