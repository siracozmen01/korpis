// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in at a portal (§9.8 of docs/08-identity.md), over the portal's own connection: an
//! invitation redeemed there registers a passkey for the portal's relying party, whose challenge
//! is answered there and nowhere else; the session it begins acts only beneath the portal's
//! organization, as every access token it is exchanged for does, whatever the account's grants
//! reach, and what it issues is held there too.

use crate::certificates::upload;
use crate::domains::child;
use crate::panel::{PANEL, accounts, refresh_cookie};
use crate::refusal::reason;
use crate::routes::attest;
use crate::support::{Harness, mutation, org_scope};
use buffa::Message;
use connectrpc::ErrorCode;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const NAME: &str = "panel.kestrel.example";

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

/// The portal, as a browser at it reaches it.
struct Portal {
    http: reqwest::Client,
    /// Where the control plane's listener answers for it.
    url: String,
    /// Its origin, which the installation's public URL's port is part of: where the panel's page
    /// at the portal is, and so what its ceremonies are made at.
    origin: String,
}

impl Portal {
    /// Calls `procedure` at the portal in Connect's unary protocol, presenting `cookie`; the
    /// answer and its headers, or the status it was refused with.
    async fn call<A: Message + Default>(
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

/// A client reaching `NAME` at `at`, trusting `root` alone.
fn reaching(at: SocketAddr, root: &CertificateDer<'static>) -> reqwest::Client {
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
        .resolve(NAME, at)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// A portal of `org` named [`NAME`], served with a provided certificate, once the control plane
/// serves it.
async fn portal_of(h: &Harness, org: &str) -> Portal {
    attest(h, "d1", org, "kestrel.example").await;
    let (root, chain, key) = issued(NAME);
    let certificate = upload(h, "c1", org, (chain, key)).await;
    let made = pb::CreatePortalRequest {
        mutation: mutation("p1").into(),
        organization: org.into(),
        name: NAME.into(),
        certificate: certificate.id,
        ..Default::default()
    };
    h.portals(h.root()).create_portal(made).await.unwrap();
    let port = h.base_url().rsplit_once(':').unwrap().1.parse::<u16>().unwrap();
    let at = SocketAddr::from(([127, 0, 0, 1], port));
    let public = PANEL.rsplit_once(':').unwrap().1;
    let portal = Portal {
        http: reaching(at, &root),
        url: format!("https://{NAME}:{port}"),
        origin: format!("https://{NAME}:{public}"),
    };
    for _ in 0..80 {
        let url = format!("{}/about/source", portal.url);
        if portal.http.get(url).send().await.is_ok() {
            return portal;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the portal was never served");
}

/// Issues `org.read` on the root organization to a new user; the invitation.
async fn invited(h: &Harness) -> String {
    let issued = pb::IssueGrantRequest {
        mutation: mutation("invite").into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(&h.root_org()).into(),
        to_new_user: pb::NewUser { display_name: "Dana".into(), ..Default::default() }.into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issued).await.unwrap().into_owned().invitation
}

/// Redeems `invitation` at `portal` with `browser`'s passkey.
async fn redeemed_at(portal: &Portal, invitation: &str, browser: &SoftAuthenticator) {
    let asked = pb::BeginRedemptionRequest {
        invitation: invitation.into(),
        name: "dana".into(),
        ..Default::default()
    };
    let answer = portal.call("korpis.v1.AccountService/BeginRedemption", &asked, None).await;
    let begun: pb::BeginRedemptionResponse = answer.unwrap().0;
    let creation = begun.creation.as_option().unwrap().clone();
    assert_eq!(creation.rp_id, NAME, "the portal's own relying party");
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
async fn answered_at(
    portal: &Portal,
    browser: &mut SoftAuthenticator,
) -> pb::SignInWithPasskeyRequest {
    let asked = pb::BeginSignInRequest::default();
    let answer = portal.call("korpis.v1.AccountService/BeginSignIn", &asked, None).await;
    let begun: pb::BeginSignInResponse = answer.unwrap().0;
    let options = begun.request.as_option().unwrap().clone();
    assert_eq!(options.rp_id, NAME);
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

#[tokio::test]
async fn a_portal_s_passkey_signs_in_there_alone_and_its_session_acts_beneath_its_organization() {
    let h = Harness::new().await;
    let reseller = child(&h, "kestrel").await;
    let portal = portal_of(&h, &reseller).await;
    let invitation = invited(&h).await;
    let mut browser = SoftAuthenticator::new(NAME, &portal.origin);
    redeemed_at(&portal, &invitation, &browser).await;

    // A challenge issued at the portal is not answered at the installation's own address.
    let elsewhere = answered_at(&portal, &mut browser).await;
    let refused = accounts(&h).sign_in_with_passkey(elsewhere).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unauthenticated);

    let access = signed_in_at(&portal, &mut browser).await;

    // The token reaches the portal's organization, wherever it is presented, and nothing above.
    let confined = h.config(Some(&access));
    let get = |id: &str| pb::GetOrganizationRequest { id: id.into(), ..Default::default() };
    h.orgs(confined.clone()).get_organization(get(&reseller)).await.unwrap();
    let above = h.orgs(confined.clone()).get_organization(get(&h.root_org())).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::NotFound, "the root the account's grant is on");
    let listed = pb::ListOrganizationsRequest { parent: Some(h.root_org()), ..Default::default() };
    let listed = h.orgs(confined.clone()).list_organizations(listed).await;
    assert_eq!(listed.unwrap_err().code, ErrorCode::NotFound, "nor what lies beneath the root");

    // What it issues is held beneath the organization too.
    let token = pb::IssueGrantRequest {
        mutation: mutation("token").into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(&h.root_org()).into(),
        to_new_token: true,
        label: "pipeline".into(),
        ..Default::default()
    };
    let token = h.grants(confined).issue_grant(token).await.unwrap_err();
    assert_eq!(reason(&token), "session.portal_confined");

    // A `korpis login` approved from the portal's session is confined as it is (Finding 129).
    let cli = h.config(Some(&cli_approved_by(&h, &access).await));
    let above = h.orgs(cli.clone()).get_organization(get(&h.root_org())).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::NotFound);
    h.orgs(cli).get_organization(get(&reseller)).await.unwrap();
}

/// Signs in at `portal` with `browser`'s passkey; an access token the session gives.
async fn signed_in_at(portal: &Portal, browser: &mut SoftAuthenticator) -> String {
    let signed_in = answered_at(portal, browser).await;
    let signed_in = portal.call("korpis.v1.AccountService/SignInWithPasskey", &signed_in, None);
    let (_, headers): (pb::SignInResponse, _) = signed_in.await.unwrap();
    let cookie = refresh_cookie(&headers).unwrap();
    let exchange = pb::ExchangeRequest::default();
    let exchanged = portal.call("korpis.v1.SessionService/Exchange", &exchange, Some(&cookie));
    let (exchanged, _): (pb::ExchangeResponse, _) = exchanged.await.unwrap();
    exchanged.access_token
}

/// A CLI's sign-in begun at the installation's address and approved with `access`; an access
/// token its session gives.
async fn cli_approved_by(h: &Harness, access: &str) -> String {
    let sessions = |b: Option<&str>| pb::SessionServiceClient::new(h.http.clone(), h.config(b));
    let begun = sessions(None).authorize_device(pb::AuthorizeDeviceRequest::default()).await;
    let begun = begun.unwrap().into_owned();
    let approve = pb::ApproveDeviceRequest { user_code: begun.user_code, ..Default::default() };
    sessions(Some(access)).approve_device(approve).await.unwrap();
    let polled = pb::DeviceTokenRequest { device_code: begun.device_code, ..Default::default() };
    let cli = sessions(None).device_token(polled).await.unwrap().into_owned();
    let request = pb::ExchangeRequest { refresh: cli.refresh, ..Default::default() };
    sessions(None).exchange(request).await.unwrap().into_owned().access_token
}
