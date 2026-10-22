// SPDX-License-Identifier: AGPL-3.0-or-later

//! ACME's TLS-ALPN-01 challenge at the proxy (RFC 8737, §3.2 of docs/07-networking.md): a client
//! offering only `acme-tls/1` for a name the proxy holds a digest for is answered with a
//! certificate made for that name carrying the digest, in `acme-tls/1`, and nothing else; the same
//! name asked for as a browser asks is served as its route serves it; and a validation of a name
//! the proxy holds no digest for reaches the workload behind it, so a `passthrough` workload
//! answers its own.

use crate::support::{Pki, Proxy, get, https, route, server_tls, workload};
use http::StatusCode;
use korpis_proto::korpis::ingress::v1 as pb;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;

/// The `acmeIdentifier` extension's OID, 1.3.6.1.5.5.7.1.31, as DER writes it.
const ACME_IDENTIFIER: [u8; 10] = [0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f];

/// Keeps the certificate a server presents and accepts it, as a validating directory reads one
/// it could not otherwise check.
#[derive(Debug, Default)]
struct Kept(Mutex<Option<CertificateDer<'static>>>);

impl ServerCertVerifier for Kept {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut kept = self.0.lock().map_err(|_| rustls::Error::General("poisoned".into()))?;
        *kept = Some(end_entity.clone().into_owned());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A validation of `name` at `at`, as a directory makes one: the certificate presented and the
/// protocol agreed, if one was, or none where no handshake completed.
async fn validate(
    at: SocketAddr,
    name: &str,
) -> Option<(CertificateDer<'static>, Option<Vec<u8>>)> {
    let kept = Arc::new(Kept::default());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(kept.clone())
        .with_no_client_auth();
    config.alpn_protocols = vec![b"acme-tls/1".to_vec()];
    let server = ServerName::try_from(name.to_owned()).unwrap();
    let tcp = TcpStream::connect(at).await.ok()?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server, tcp).await.ok()?;
    let protocol = tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    let cert = kept.0.lock().unwrap().take()?;
    Some((cert, protocol))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn a_validation_of_a_name_the_proxy_holds_a_digest_for_is_answered_with_it_alone() {
    let pki = Pki::new();
    let shop = workload("shop", None).await;
    let digest = vec![0x5a; 32];
    let proxy = Proxy::start(&pb::IngressConfig {
        routes: vec![route(&["shop.example.test"], shop, pb::RouteMode::ROUTE_MODE_HTTP, "crt")],
        certificates: vec![pki.certificate("crt", &["shop.example.test"])],
        alpn_challenges: vec![pb::AlpnChallenge {
            name: "Shop.Example.test".into(),
            digest: digest.clone(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let (cert, protocol) = validate(proxy.https, "shop.example.test").await.expect("answered");
    assert_eq!(protocol.as_deref(), Some(&b"acme-tls/1"[..]));
    let der = cert.as_ref();
    assert!(contains(der, &ACME_IDENTIFIER), "the acmeIdentifier extension");
    let mut value = vec![0x04, 0x20];
    value.extend_from_slice(&digest);
    assert!(contains(der, &value), "carrying the digest as an OCTET STRING");
    assert!(contains(der, b"shop.example.test"), "for the name alone");

    let (status, body) =
        https(&pki, proxy.https, "shop.example.test", true, get("/", "shop.example.test")).await;
    assert_eq!(status, StatusCode::OK, "a browser is served as before: {body}");
}

#[tokio::test]
async fn a_validation_of_a_name_the_proxy_holds_no_digest_for_reaches_its_workload() {
    let pki = Pki::new();
    let own = server_tls(&pki.leaf(&["tls.example.test"]));
    let backend = workload("own-tls", Some(own)).await;
    let proxy = Proxy::start(&pb::IngressConfig {
        routes: vec![route(
            &["tls.example.test"],
            backend,
            pb::RouteMode::ROUTE_MODE_PASSTHROUGH,
            "",
        )],
        ..Default::default()
    });
    // The workload's own certificate is presented, and no protocol agreed, since it speaks
    // none: the validation was the workload's to answer, not the proxy's.
    let (cert, protocol) = validate(proxy.https, "tls.example.test").await.expect("its workload");
    assert_eq!(protocol, None);
    assert!(!contains(cert.as_ref(), &ACME_IDENTIFIER), "the workload's own certificate");
    assert!(validate(proxy.https, "nobody.example.test").await.is_none(), "an unrouted name");
}
