// SPDX-License-Identifier: AGPL-3.0-or-later

//! A portal as a browser at it reaches the control plane (§3.6 of docs/07-networking.md): over a
//! connection naming the portal in SNI, verified for its provided certificate, calling the API in
//! Connect's unary protocol at the portal's own name.

use crate::certificates::upload;
use crate::panel::{PANEL, refresh_cookie};
use crate::routes::attest;
use crate::support::{Harness, mutation};
use buffa::Message;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;
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
    /// Its name, the identifier of its relying party.
    pub name: String,
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
    let made = pb::CreatePortalRequest {
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
        name: name.into(),
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

/// Redeems `invitation` at `portal` as `dana` with `browser`'s passkey, registered for the
/// portal's own relying party.
pub async fn redeemed_at(portal: &Portal, invitation: &str, browser: &SoftAuthenticator) {
    let asked = pb::BeginRedemptionRequest {
        invitation: invitation.into(),
        name: "dana".into(),
        ..Default::default()
    };
    let answer = portal.call("korpis.v1.AccountService/BeginRedemption", &asked, None).await;
    let begun: pb::BeginRedemptionResponse = answer.unwrap().0;
    let creation = begun.creation.as_option().unwrap().clone();
    assert_eq!(creation.rp_id, portal.name, "the portal's own relying party");
    let created = browser.create(&creation.challenge);
    let registration = pb::PasskeyRegistration {
        challenge_id: creation.challenge_id,
        label: "phone".into(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
        ..Default::default()
    };
    let redeem = pb::RedeemWithPasskeyRequest {
        invitation: invitation.into(),
        name: "dana".into(),
        registration: registration.into(),
        ..Default::default()
    };
    let done = portal.call("korpis.v1.AccountService/RedeemWithPasskey", &redeem, None).await;
    let _: pb::RedeemResponse = done.unwrap().0;
}

/// A sign-in challenge issued at `portal`, answered by `browser`.
pub async fn answered_at(
    portal: &Portal,
    browser: &mut SoftAuthenticator,
) -> pb::SignInWithPasskeyRequest {
    let asked = pb::BeginSignInRequest::default();
    let answer = portal.call("korpis.v1.AccountService/BeginSignIn", &asked, None).await;
    let begun: pb::BeginSignInResponse = answer.unwrap().0;
    let options = begun.request.as_option().unwrap().clone();
    assert_eq!(options.rp_id, portal.name);
    let got = browser.get(&options.challenge);
    pb::SignInWithPasskeyRequest {
        challenge_id: options.challenge_id,
        credential_id: got.credential_id,
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
        ..Default::default()
    }
}

/// Signs in at `portal` with `browser`'s passkey; the session's refresh credential, and an
/// access token it gives.
pub async fn signed_in_at(portal: &Portal, browser: &mut SoftAuthenticator) -> (String, String) {
    let signed_in = answered_at(portal, browser).await;
    let signed_in = portal.call("korpis.v1.AccountService/SignInWithPasskey", &signed_in, None);
    let (_, headers): (pb::SignInResponse, _) = signed_in.await.unwrap();
    let cookie = refresh_cookie(&headers).unwrap();
    let exchange = pb::ExchangeRequest::default();
    let exchanged = portal.call("korpis.v1.SessionService/Exchange", &exchange, Some(&cookie));
    let (exchanged, _): (pb::ExchangeResponse, _) = exchanged.await.unwrap();
    (cookie, exchanged.access_token)
}
