// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provided certificates (§3.2 of docs/07-networking.md): uploaded chain and key, the key shown
//! to be the leaf's and never answered; named by an endpoint only if they are its organization's,
//! valid, and cover every name it routes; replaced in place only by a chain still covering what
//! is served with them; and deleted only once no endpoint serves with them.

use crate::domains::child;
use crate::recipes::{change, project, push_paper};
use crate::routes::{attest, declare, refused, web};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::v1 as pb;

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
