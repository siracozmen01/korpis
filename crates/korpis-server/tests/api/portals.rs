// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals (§3.6 of docs/07-networking.md): a name its organization holds, routed by no endpoint
//! and no other portal's, served with its own `acme` certificate or a provided one covering it;
//! kept from being taken by a domain's deletion or delegation, and from any endpoint's route.

use crate::certificates::{issued, upload};
use crate::domains::child;
use crate::recipes::{project, push_paper};
use crate::routes::{attest, declare, refused, web};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::v1 as pb;

use pb::CertificateSource as Source;
use pb::CertificateState as State;

fn asked(key: &str, org: &str, name: &str, certificate: &str) -> pb::CreatePortalRequest {
    pb::CreatePortalRequest {
        mutation: mutation(key).into(),
        organization: org.into(),
        name: name.into(),
        certificate: certificate.into(),
        ..Default::default()
    }
}

async fn made(h: &Harness, req: pb::CreatePortalRequest) -> pb::Portal {
    let out = h.portals(h.root()).create_portal(req).await.unwrap().into_owned();
    out.portal.into_option().unwrap()
}

async fn refusal(h: &Harness, req: pb::CreatePortalRequest) -> String {
    reason(&h.portals(h.root()).create_portal(req).await.unwrap_err())
}

#[tokio::test]
async fn a_portal_is_a_name_its_organization_holds_and_nothing_else_has() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let reseller = child(&h, "pinecone").await;
    attest(&h, "d1", &reseller, "pinecone.example").await;
    let shops = project(&h, &reseller, "shops").await;
    declare(&h, &shops, "shop", web(&repo, &["shop.pinecone.example"])).await;

    let root = h.root_org();
    let not_held = asked("p0", &root, "panel.pinecone.example", "");
    assert_eq!(refusal(&h, not_held).await, "portal.name_not_held");
    let routed = asked("p1", &reseller, "shop.pinecone.example", "");
    assert_eq!(refusal(&h, routed).await, "portal.name_routed");

    let portal = made(&h, asked("p2", &reseller, "panel.pinecone.example", "")).await;
    assert_eq!(portal.organization, reseller);
    assert_eq!(portal.certificate_source.as_known(), Some(Source::CERTIFICATE_SOURCE_ACME));
    assert_eq!(portal.certificate_state.as_known(), Some(State::CERTIFICATE_STATE_PENDING));
    let again = asked("p3", &reseller, "panel.pinecone.example", "");
    assert_eq!(refusal(&h, again).await, "portal.taken");

    // No endpoint may route it, at declaration or after.
    let taken = web(&repo, &["panel.pinecone.example"]);
    assert_eq!(refused(&h, &shops, "squat", taken).await, "endpoint.name_portal");

    let read = h.portals(h.root());
    let got = read
        .get_portal(pb::GetPortalRequest { id: portal.id.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(got.portal.into_option().unwrap().name, "panel.pinecone.example");
    let listed = read
        .list_portals(pb::ListPortalsRequest { organization: reseller, ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.portals.len(), 1);
}

#[tokio::test]
async fn a_portal_keeps_its_name_through_deletion_and_delegation() {
    let h = Harness::new().await;
    let root = h.root_org();
    let apex = attest(&h, "d1", &root, "kestrel.example").await;
    made(&h, asked("p1", &root, "panel.kestrel.example", "")).await;

    let customer = child(&h, "blockhaven").await;
    let delegated = pb::CreateDomainRequest {
        mutation: mutation("d2").into(),
        organization: customer,
        name: "panel.kestrel.example".into(),
        verification: pb::DomainVerification::DOMAIN_VERIFICATION_DELEGATED.into(),
        parent: Some(apex.id.clone()),
        ..Default::default()
    };
    let taken = h.domains(h.root()).create_domain(delegated).await.unwrap_err();
    assert_eq!(reason(&taken), "domain.portal");

    let gone = pb::DeleteDomainRequest {
        mutation: mutation("d3").into(),
        id: apex.id,
        ..Default::default()
    };
    let kept = h.domains(h.root()).delete_domain(gone).await.unwrap_err();
    assert_eq!(reason(&kept), "domain.portal");
}

#[tokio::test]
async fn a_provided_certificate_serves_a_portal_while_it_covers_the_name() {
    let h = Harness::new().await;
    let root = h.root_org();
    attest(&h, "d1", &root, "kestrel.example").await;
    let other = upload(&h, "c1", &root, issued(&["shop.kestrel.example"])).await;
    let covering = upload(&h, "c2", &root, issued(&["*.kestrel.example"])).await;

    let wrong = asked("p1", &root, "panel.kestrel.example", &other.id);
    assert_eq!(refusal(&h, wrong).await, "portal.certificate");
    let portal = made(&h, asked("p2", &root, "panel.kestrel.example", &covering.id)).await;
    assert_eq!(portal.certificate, covering.id);
    assert_eq!(portal.certificate_state.as_known(), Some(State::CERTIFICATE_STATE_ISSUED));

    let certificates = h.certificates(h.root());
    let delete = |key: &str| pb::DeleteCertificateRequest {
        mutation: mutation(key).into(),
        id: covering.id.clone(),
        ..Default::default()
    };
    let in_use = certificates.delete_certificate(delete("c3")).await.unwrap_err();
    assert_eq!(reason(&in_use), "certificate.in_use");
    let (chain, key) = issued(&["shop.kestrel.example"]);
    let replace = pb::ReplaceCertificateRequest {
        mutation: mutation("c4").into(),
        id: covering.id.clone(),
        chain,
        key,
        ..Default::default()
    };
    let uncovered = certificates.replace_certificate(replace).await.unwrap_err();
    assert_eq!(reason(&uncovered), "certificate.uncovered");

    // Served with its own certificate again, the provided one is free to go.
    let own = pb::SetPortalCertificateRequest {
        mutation: mutation("p3").into(),
        id: portal.id.clone(),
        ..Default::default()
    };
    let changed = h.portals(h.root()).set_portal_certificate(own).await.unwrap().into_owned();
    let changed = changed.portal.into_option().unwrap();
    assert_eq!(changed.certificate_source.as_known(), Some(Source::CERTIFICATE_SOURCE_ACME));
    certificates.delete_certificate(delete("c5")).await.unwrap();

    let gone = pb::DeletePortalRequest {
        mutation: mutation("p4").into(),
        id: portal.id.clone(),
        ..Default::default()
    };
    h.portals(h.root()).delete_portal(gone).await.unwrap();
    let after = pb::GetPortalRequest { id: portal.id, ..Default::default() };
    assert!(h.portals(h.root()).get_portal(after).await.is_err());
}
