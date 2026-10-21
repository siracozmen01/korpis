// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serving a portal (§3.6 of docs/07-networking.md), validated for real: Pebble resolves the
//! portal's name to this machine and proves it by TLS-ALPN-01 against the control plane's own
//! listener, which answers from the challenge the issuer recorded; the certificate issued is then
//! served to a client naming the portal, which verifies it against Pebble's issuing root; a
//! request naming another host on its connection is refused 421; and a cross-origin call is
//! answered for the portal's own origin alone.

use crate::panel::PANEL;
use crate::routes::attest;
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_server::testing::{ChallengeDns, Pebble, PebbleOptions};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

const NAME: &str = "panel.kestrel.example";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The portal's state, read until its certificate is issued.
async fn issued(h: &Harness, id: &str) -> pb::Portal {
    for _ in 0..240 {
        let got = pb::GetPortalRequest { id: id.into(), ..Default::default() };
        let p = h.portals(h.root()).get_portal(got).await.unwrap().into_owned();
        let p = p.portal.into_option().unwrap();
        if p.certificate_state.as_known() == Some(pb::CertificateState::CERTIFICATE_STATE_ISSUED) {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the portal's certificate was never issued");
}

/// A TLS client trusting `roots` alone.
fn trusting(roots: &[CertificateDer<'static>]) -> rustls::ClientConfig {
    let mut store = rustls::RootCertStore::empty();
    for r in roots {
        store.add(r.clone()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth()
}

/// The root Pebble issues its certificates under, read from its management interface.
async fn issuing_root(pebble: &Pebble) -> CertificateDer<'static> {
    let own = CertificateDer::from_pem_file(&pebble.ca).unwrap();
    let client = reqwest::Client::builder().use_preconfigured_tls(trusting(&[own])).build();
    let url = format!("{}/roots/0", pebble.management);
    let pem = client.unwrap().get(url).send().await.unwrap().text().await.unwrap();
    CertificateDer::from_pem_slice(pem.as_bytes()).unwrap()
}

/// A client reaching the portal's name at the control plane, trusting `root` alone.
fn client(at: SocketAddr, root: &CertificateDer<'static>) -> reqwest::Client {
    reqwest::Client::builder()
        .use_preconfigured_tls(trusting(std::slice::from_ref(root)))
        .resolve(NAME, at)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_portal_is_proven_by_tls_alpn_and_served_under_its_own_name() {
    let port = free_port();
    let Ok(dns) = ChallengeDns::start(Ipv4Addr::LOCALHOST).await else { return };
    let options =
        PebbleOptions { tls_port: Some(port), dns: Some(dns.address), ..PebbleOptions::default() };
    let Some(pebble) = crate::pebble::start_with(options).await else { return };
    let at = SocketAddr::from(([127, 0, 0, 1], port));
    let h = Harness::with_acme_on(pebble.config(), at).await;
    attest(&h, "d1", &h.root_org(), "kestrel.example").await;
    let made = pb::CreatePortalRequest {
        mutation: mutation("p1").into(),
        organization: h.root_org(),
        name: NAME.into(),
        ..Default::default()
    };
    let made = h.portals(h.root()).create_portal(made).await.unwrap().into_owned();
    let portal = issued(&h, &made.portal.into_option().unwrap().id).await;
    assert!(portal.certificate_reason.is_empty(), "{}", portal.certificate_reason);

    // The listener serves it, verified for the portal's name, once it has read the portals again.
    let root = issuing_root(&pebble).await;
    let url = format!("https://{NAME}:{port}/about/source");
    let mut served = None;
    for _ in 0..80 {
        if let Ok(r) = client(at, &root).get(&url).send().await {
            served = Some(r.status());
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(served, Some(reqwest::StatusCode::OK), "the portal's own certificate, served");

    let other = client(at, &root).get(&url).header("host", "korpis.kestrel.example");
    let other = other.send().await.unwrap();
    assert_eq!(other.status(), reqwest::StatusCode::MISDIRECTED_REQUEST);

    let origin = format!("https://{NAME}:{}", PANEL.rsplit_once(':').unwrap().1);
    let procedure = format!("https://{NAME}:{port}/korpis.v1.ServerInfoService/GetServerInfo");
    let preflight = |o: &str| {
        client(at, &root)
            .request(reqwest::Method::OPTIONS, &procedure)
            .header("origin", o)
            .header("access-control-request-method", "POST")
    };
    let own = preflight(&origin).send().await.unwrap();
    assert_eq!(own.status(), reqwest::StatusCode::NO_CONTENT, "its own origin");
    let installation = preflight(PANEL).send().await.unwrap();
    assert_eq!(installation.status(), reqwest::StatusCode::FORBIDDEN, "no other, at a portal");
}
