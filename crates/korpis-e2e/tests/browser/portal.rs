// SPDX-License-Identifier: AGPL-3.0-or-later

//! A reseller's portal for the browser to sign in at (§3.6 of docs/07-networking.md, trace 48 of
//! docs/23-walkthroughs.md). Pinecone, beneath the root organization, holds `pinecone.localhost`,
//! which Chromium resolves to this machine as it does every name beneath `localhost`, and serves
//! `panel.pinecone.localhost` with a certificate of its own, provided; its theme names the panel
//! "Pinecone Panel", and its landing screen welcomes those it serves; and Ada is invited with a
//! grant on the root organization, beyond Pinecone, so the session she begins at the portal is
//! confined beneath what her grant reaches.

use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const NAME: &str = "panel.pinecone.localhost";

/// The portal, as the browser tests are told of it.
pub struct Portal {
    url: String,
    invitation: String,
}

impl Portal {
    /// Tells the browser tests where the portal is and what Ada redeems there.
    pub fn env(&self, playwright: &mut tokio::process::Command) {
        playwright.env("KORPIS_E2E_PORTAL_URL", &self.url);
        playwright.env("KORPIS_E2E_PORTAL_INVITATION", &self.invitation);
    }
}

/// A test authority, and a certificate it issued for `name` with its key, in PEM.
fn issued(name: &str) -> (CertificateDer<'static>, String, String) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let authority = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
    let leaf = leaf.signed_by(&key, &issuer).unwrap();
    (authority.der().clone(), leaf.pem(), key.serialize_pem())
}

/// Pinecone, beneath the root organization, dressed in its theme; its identifier.
async fn reseller(cp: &ControlPlane) -> String {
    let orgs = pb::OrganizationServiceClient::new(cp.http(), cp.config());
    let made = pb::CreateOrganizationRequest {
        mutation: mutation("pinecone").into(),
        name: "pinecone".into(),
        parent: Some(cp.root_org.clone()),
        ..Default::default()
    };
    let made = orgs.create_organization(made).await.unwrap().into_owned();
    let id = made.organization.into_option().unwrap().id;
    let theme = pb::Theme { product_name: "Pinecone Panel".into(), ..Default::default() };
    let themed = pb::SetThemeRequest {
        mutation: mutation("pinecone-theme").into(),
        organization: id.clone(),
        theme: Some(theme).into(),
        ..Default::default()
    };
    orgs.set_theme(themed).await.unwrap();
    orgs.set_layout(landing(&id)).await.unwrap();
    id
}

/// Pinecone's landing screen: a welcome and a link to its support desk, above what is reached.
fn landing(org: &str) -> pb::SetLayoutRequest {
    use korpis_proto::korpis::v1::__buffa::oneof::layout_item::Kind as K;
    let item = |kind| pb::LayoutItem { kind: Some(kind), ..Default::default() };
    let welcome = pb::LayoutNotice {
        heading: "Welcome to Pinecone".into(),
        text: "Your servers are below.".into(),
        ..Default::default()
    };
    let support = pb::LayoutLink {
        label: "Pinecone support".into(),
        url: "https://help.pinecone.localhost/".into(),
        ..Default::default()
    };
    let items = vec![
        item(K::Notice(Box::new(welcome))),
        item(K::Places(Box::default())),
        item(K::Link(Box::new(support))),
    ];
    pb::SetLayoutRequest {
        mutation: mutation("pinecone-layout").into(),
        organization: org.into(),
        layout: pb::Layout { items, ..Default::default() }.into(),
        ..Default::default()
    }
}

/// Pinecone's portal, served with a provided certificate; the authority that issued it.
async fn served(cp: &ControlPlane, org: &str) -> CertificateDer<'static> {
    let domain = pb::CreateDomainRequest {
        mutation: mutation("pinecone-domain").into(),
        organization: org.into(),
        name: "pinecone.localhost".into(),
        verification: pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR.into(),
        ..Default::default()
    };
    pb::DomainServiceClient::new(cp.http(), cp.config()).create_domain(domain).await.unwrap();
    let (authority, chain, key) = issued(NAME);
    let certificate = pb::CreateCertificateRequest {
        mutation: mutation("pinecone-certificate").into(),
        organization: org.into(),
        chain,
        key,
        ..Default::default()
    };
    let certificates = pb::CertificateServiceClient::new(cp.http(), cp.config());
    let made = certificates.create_certificate(certificate).await.unwrap().into_owned();
    let portal = pb::CreatePortalRequest {
        mutation: mutation("pinecone-portal").into(),
        organization: org.into(),
        name: NAME.into(),
        certificate: made.certificate.into_option().unwrap().id,
        ..Default::default()
    };
    pb::PortalServiceClient::new(cp.http(), cp.config()).create_portal(portal).await.unwrap();
    authority
}

/// Waits until the control plane serves `url`'s portal with its certificate, `authority`'s.
async fn answering(url: &str, at: SocketAddr, authority: &CertificateDer<'static>) {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(authority.clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client = reqwest::Client::builder().use_preconfigured_tls(tls).resolve(NAME, at).build();
    let client = client.unwrap();
    for _ in 0..80 {
        if client.get(format!("{url}/about/source")).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the portal was never served");
}

/// Pinecone's portal, served, and Ada's invitation.
pub async fn pinecone(cp: &ControlPlane) -> Portal {
    let org = reseller(cp).await;
    let authority = served(cp, &org).await;
    let port = cp.url.rsplit_once(':').unwrap().1.parse::<u16>().unwrap();
    let url = format!("https://{NAME}:{port}");
    answering(&url, SocketAddr::from(([127, 0, 0, 1], port)), &authority).await;
    let scope = pb::Scope {
        kind: Some(pb::scope::Kind::Organization(cp.root_org.clone())),
        ..Default::default()
    };
    let invite = pb::IssueGrantRequest {
        mutation: mutation("ada").into(),
        actions: vec!["org.read".into()],
        scope: scope.into(),
        to_new_user: pb::NewUser { display_name: "Ada".into(), ..Default::default() }.into(),
        ..Default::default()
    };
    let grants = pb::GrantServiceClient::new(cp.http(), cp.config());
    let invitation = grants.issue_grant(invite).await.unwrap().into_owned().invitation;
    Portal { url, invitation }
}
