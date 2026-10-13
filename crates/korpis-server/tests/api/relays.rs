// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays running as their own process (§3.1 of docs/02-architecture.md): enrolled over the root
//! organization with a one-use token, joined once with the relay's keys, given a certificate for
//! their URL's host under the installation's authority, listed, and removed.

use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_server::standalone::{JoinError, RelayDir, join};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier as _;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::sync::Arc;

async fn token(
    h: &Harness,
    org: &str,
    name: &str,
) -> Result<pb::CreateRelayEnrollmentTokenResponse, connectrpc::ConnectError> {
    h.nodes(h.root())
        .create_relay_enrollment_token(pb::CreateRelayEnrollmentTokenRequest {
            mutation: mutation(&format!("relay-{name}")).into(),
            organization: org.to_owned(),
            name: name.to_owned(),
            url: "https://localhost:9443".into(),
            ..Default::default()
        })
        .await
        .map(connectrpc::client::UnaryResponse::into_owned)
}

/// Whether the chain in `dir` verifies for `host` under the authority it pinned.
fn verifies(dir: &RelayDir, host: &str) -> bool {
    let ca = CertificateDer::from_pem_file(dir.file("ca.pem")).unwrap();
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(dir.file("tls.pem")).unwrap().map(Result::unwrap).collect();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build().unwrap();
    let name = ServerName::try_from(host.to_owned()).unwrap();
    let [leaf, rest @ ..] = chain.as_slice() else { return false };
    verifier.verify_server_cert(leaf, rest, &name, &[], UnixTime::now()).is_ok()
}

#[tokio::test]
async fn a_relay_joins_once_with_a_certificate_for_its_host_and_can_be_removed() {
    let h = Harness::new().await;
    let created = token(&h, &h.root_org(), "edge-1").await.unwrap();
    assert!(created.ca_fingerprint.starts_with("sha256:"));
    let dir = tempfile::tempdir().unwrap();
    let relay = RelayDir::new(dir.path().join("relay"));
    let id = join(&relay, &h.base_url(), &created.token, &created.ca_fingerprint).await.unwrap();
    assert!(id.starts_with("rly_"));
    assert!(verifies(&relay, "localhost"), "a certificate the agents' authority vouches for");
    assert!(!verifies(&relay, "elsewhere.example"), "for its own host only");

    let again = join(&relay, &h.base_url(), &created.token, &created.ca_fingerprint).await;
    assert!(matches!(again, Err(JoinError::Joined(_))), "a relay joins once");
    std::fs::remove_file(relay.file("relay")).unwrap();
    let retried = join(&relay, &h.base_url(), &created.token, &created.ca_fingerprint).await;
    assert_eq!(retried.unwrap(), id, "a lost answer, asked again with the same keys");
    let stranger = RelayDir::new(dir.path().join("stranger"));
    let stolen = join(&stranger, &h.base_url(), &created.token, &created.ca_fingerprint).await;
    assert!(matches!(stolen, Err(JoinError::Refused(_))), "another key is refused");

    let nodes = h.nodes(h.root());
    let list = || {
        nodes
            .list_relays(pb::ListRelaysRequest { organization: h.root_org(), ..Default::default() })
    };
    let listed = list().await.unwrap().into_owned().relays;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].id.as_str(), listed[0].url.as_str()),
        (id.as_str(), "https://localhost:9443")
    );
    assert!(listed[0].certificate_expires_at.as_option().is_some());
    nodes
        .remove_relay(pb::RemoveRelayRequest {
            mutation: mutation("remove").into(),
            organization: h.root_org(),
            relay: id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let listed = list().await.unwrap().into_owned().relays;
    assert!(listed[0].removed_at.as_option().is_some());
}

#[tokio::test]
async fn relays_belong_to_the_root_organization() {
    let h = Harness::new().await;
    let created = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("reseller").into(),
            name: "reseller".into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let child = created.organization.as_option().unwrap().id.clone();
    let e = token(&h, &child, "edge-2").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "{e:?}");
}
