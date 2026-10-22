// SPDX-License-Identifier: AGPL-3.0-or-later

//! An endpoint's client certificates (§3.2 of docs/07-networking.md): the authorities a
//! declaration gives in PEM read back as the certificates they are, and anything that is not one
//! of them, a key above all, refused rather than skipped.

use korpis_core::workload::spec::endpoints::{ClientCertificates, TlsSpec};
use korpis_proto::korpis::v1 as pb;
use korpis_wire::spec::endpoints::{endpoint_from_pb, endpoint_to_pb};

/// An authority's certificate and its key, in PEM.
fn authority() -> (String, String) {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    (cert.pem(), key.serialize_pem())
}

/// An ingress endpoint asking for client certificates as `asked`, trusting `ca`.
fn locked(asked: pb::ClientCertificates, ca: &str) -> pb::EndpointSpec {
    pb::EndpointSpec {
        name: "api".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTPS.into(),
        container_port: 8080,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec!["api.example.com".into()],
        tls: pb::TlsSpec {
            source: pb::TlsSource::TLS_SOURCE_ACME.into(),
            client_certificates: asked.into(),
            client_ca: ca.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn refused(e: &pb::EndpointSpec) -> String {
    endpoint_from_pb("endpoints[0]", e).unwrap_err().field
}

#[test]
fn authorities_read_back_as_the_certificates_they_are() {
    let ((one, _), (two, _)) = (authority(), authority());
    let declared = locked(pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED, &(one + &two));
    let read = endpoint_from_pb("endpoints[0]", &declared).unwrap();
    let Some(TlsSpec::Acme { clients: Some(clients) }) = &read.tls else { panic!("{read:?}") };
    assert_eq!(clients.certificates, ClientCertificates::Required);
    assert_eq!(clients.authorities.len(), 2);
    let written = endpoint_to_pb(&read);
    assert_eq!(endpoint_from_pb("endpoints[0]", &written).unwrap(), read, "written as read");
    let optional = locked(pb::ClientCertificates::CLIENT_CERTIFICATES_OPTIONAL, &two);
    let read = endpoint_from_pb("endpoints[0]", &optional).unwrap();
    let Some(TlsSpec::Acme { clients: Some(clients) }) = &read.tls else { panic!("{read:?}") };
    assert_eq!(clients.certificates, ClientCertificates::Optional);
}

#[test]
fn anything_among_the_authorities_that_is_not_a_certificate_is_refused() {
    let (ca, key) = authority();
    let required = pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED;
    let at = "endpoints[0].tls.client_ca";
    assert_eq!(refused(&locked(required, &format!("{ca}{key}"))), at, "a key pasted in");
    let openssh = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
    assert_eq!(refused(&locked(required, &format!("{ca}{openssh}"))), at, "a label PEM lacks");
    let junk = "-----BEGIN CERTIFICATE-----\nMAMCAQA=\n-----END CERTIFICATE-----\n";
    assert_eq!(refused(&locked(required, junk)), at, "DER that is not a certificate");
    assert_eq!(refused(&locked(required, &ca.repeat(400))), at, "beyond 64 KiB");
    let unasked = locked(pb::ClientCertificates::CLIENT_CERTIFICATES_UNSPECIFIED, &ca);
    assert_eq!(refused(&unasked), "endpoints[0].tls.client_certificates");
    let mut passthrough = locked(required, &ca);
    passthrough.tls.get_or_insert_default().source = pb::TlsSource::TLS_SOURCE_PASSTHROUGH.into();
    assert_eq!(refused(&passthrough), "endpoints[0].tls.client_certificates");
}
