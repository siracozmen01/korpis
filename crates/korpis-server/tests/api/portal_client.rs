// SPDX-License-Identifier: AGPL-3.0-or-later

//! A portal as a browser at it reaches the control plane (§3.6 of docs/07-networking.md): over a
//! connection naming the portal in SNI, verified for its provided certificate, calling the API in
//! Connect's unary protocol at the portal's own name.

use crate::certificates::upload;
use crate::panel::PANEL;
use crate::routes::attest;
use crate::support::{Harness, mutation};
use buffa::Message;
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

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

/// A client reaching `name` at `at`, trusting `root` alone.
fn reaching(name: &str, at: SocketAddr, root: &CertificateDer<'static>) -> reqwest::Client {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(root.clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .resolve(name, at)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// A portal, as a browser at it reaches it.
pub struct Portal {
    http: reqwest::Client,
    /// Its identifier, `ptl_…`.
    pub id: String,
    /// Where the control plane's listener answers for it.
    url: String,
    /// Its origin, which the installation's public URL's port is part of: where the panel's page
    /// at the portal is, and so what its ceremonies are made at.
    pub origin: String,
}

impl Portal {
    /// Calls `procedure` at the portal in Connect's unary protocol, presenting `cookie`; the
    /// answer and its headers, or the status it was refused with.
    pub async fn call<A: Message + Default>(
        &self,
        procedure: &str,
        request: &impl Message,
        cookie: Option<&str>,
    ) -> Result<(A, reqwest::header::HeaderMap), reqwest::StatusCode> {
        let mut sent = self
            .http
            .post(format!("{}/{procedure}", self.url))
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request.encode_to_vec());
        if let Some(c) = cookie {
            sent = sent.header("cookie", format!("__Host-korpis_refresh={c}"));
        }
        let answer = sent.send().await.unwrap();
        if !answer.status().is_success() {
            return Err(answer.status());
        }
        let headers = answer.headers().clone();
        Ok((A::decode_from_slice(&answer.bytes().await.unwrap()).unwrap(), headers))
    }
}

/// A portal of `org` named `name` beneath its verified domain `domain`, served with a provided
/// certificate, once the control plane serves it; `key` keeps its mutations apart from another's.
pub async fn portal_of(
    h: &Harness,
    (key, org): (&str, &str),
    (domain, name): (&str, &str),
) -> Portal {
    attest(h, &format!("{key}-domain"), org, domain).await;
    let (root, chain, pem) = issued(name);
    let certificate = upload(h, &format!("{key}-certificate"), org, (chain, pem)).await;
    let made = korpis_proto::korpis::v1::CreatePortalRequest {
        mutation: mutation(&format!("{key}-portal")).into(),
        organization: org.into(),
        name: name.into(),
        certificate: certificate.id,
        ..Default::default()
    };
    let made = h.portals(h.root()).create_portal(made).await.unwrap().into_owned();
    let port = h.base_url().rsplit_once(':').unwrap().1.parse::<u16>().unwrap();
    let public = PANEL.rsplit_once(':').unwrap().1;
    let portal = Portal {
        http: reaching(name, SocketAddr::from(([127, 0, 0, 1], port)), &root),
        id: made.portal.into_option().unwrap().id,
        url: format!("https://{name}:{port}"),
        origin: format!("https://{name}:{public}"),
    };
    for _ in 0..80 {
        if portal.http.get(format!("{}/about/source", portal.url)).send().await.is_ok() {
            return portal;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the portal was never served");
}
