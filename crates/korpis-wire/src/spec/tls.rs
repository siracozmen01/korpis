// SPDX-License-Identifier: AGPL-3.0-or-later

//! An endpoint's TLS (§3.2 of docs/07-networking.md): where its certificate comes from; for TLS
//! the proxy terminates, the client certificates it asks for and the authorities, in PEM, it
//! verifies them against; and for TLS it passes through, whether its workload is sent a PROXY
//! header, which the wire declares as the endpoint's `client_address`.
//!
//! The authorities are read here as the proxy reads them: every PEM section must be a
//! certificate `webpki` takes as a trust anchor, so a declaration admitted is one the proxy can
//! serve, and a private key pasted among them is refused rather than skipped and published.

use crate::{Result, invalid};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use korpis_core::workload::spec::endpoints::{
    ClientAuth, ClientCertificates, MAX_CLIENT_AUTHORITY_BYTES, TlsSpec,
};
use korpis_proto::korpis::v1 as pb;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::{PemObject as _, SectionKind};

/// The wire form of `t`.
pub(crate) fn tls_to_pb(t: &TlsSpec) -> pb::TlsSpec {
    let (source, secret, clients) = match t {
        TlsSpec::Acme { clients } => (pb::TlsSource::TLS_SOURCE_ACME, String::new(), clients),
        TlsSpec::Provided { secret, clients } => {
            (pb::TlsSource::TLS_SOURCE_PROVIDED, secret.clone(), clients)
        }
        TlsSpec::Passthrough { .. } => {
            (pb::TlsSource::TLS_SOURCE_PASSTHROUGH, String::new(), &None)
        }
    };
    let (client_certificates, client_ca) = clients.as_ref().map_or_else(
        || (pb::ClientCertificates::CLIENT_CERTIFICATES_UNSPECIFIED, String::new()),
        |c| (certificates_to_pb(c.certificates), pem(&c.authorities)),
    );
    pb::TlsSpec {
        source: source.into(),
        secret,
        client_certificates: client_certificates.into(),
        client_ca,
        ..Default::default()
    }
}

/// `t` read from the field `field`.
pub(crate) fn tls_from_pb(field: &str, t: &pb::TlsSpec) -> Result<TlsSpec> {
    let clients = clients_from_pb(field, t)?;
    match t.source.as_known() {
        Some(pb::TlsSource::TLS_SOURCE_ACME) => Ok(TlsSpec::Acme { clients }),
        Some(pb::TlsSource::TLS_SOURCE_PROVIDED) => {
            Ok(TlsSpec::Provided { secret: t.secret.clone(), clients })
        }
        Some(pb::TlsSource::TLS_SOURCE_PASSTHROUGH) if clients.is_none() => {
            Ok(TlsSpec::Passthrough { proxy_protocol: false })
        }
        Some(pb::TlsSource::TLS_SOURCE_PASSTHROUGH) => Err(invalid(
            &format!("{field}.client_certificates"),
            "a passthrough workload terminates TLS, and asks for certificates itself",
        )),
        _ => Err(invalid(&format!("{field}.source"), "expected a TLS source")),
    }
}

fn certificates_to_pb(c: ClientCertificates) -> pb::ClientCertificates {
    match c {
        ClientCertificates::Required => pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED,
        ClientCertificates::Optional => pb::ClientCertificates::CLIENT_CERTIFICATES_OPTIONAL,
    }
}

/// The client certificates `t` asks for, none where it asks for none.
fn clients_from_pb(field: &str, t: &pb::TlsSpec) -> Result<Option<ClientAuth>> {
    let certificates = match t.client_certificates.as_known() {
        Some(pb::ClientCertificates::CLIENT_CERTIFICATES_UNSPECIFIED) if t.client_ca.is_empty() => {
            return Ok(None);
        }
        Some(pb::ClientCertificates::CLIENT_CERTIFICATES_UNSPECIFIED) => {
            let why = "authorities are trusted only where client certificates are asked for";
            return Err(invalid(&format!("{field}.client_certificates"), why));
        }
        Some(pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED) => ClientCertificates::Required,
        Some(pb::ClientCertificates::CLIENT_CERTIFICATES_OPTIONAL) => ClientCertificates::Optional,
        None => {
            return Err(invalid(&format!("{field}.client_certificates"), "required or optional"));
        }
    };
    let authorities = authorities(&format!("{field}.client_ca"), &t.client_ca)?;
    Ok(Some(ClientAuth { certificates, authorities }))
}

/// The client address `e` declares, as its TLS: `proxy_protocol` is declared on an endpoint
/// whose TLS is passed through and on no other, and the other ways a workload sees its client
/// are the platform's to say, never declared (§3.2 and §3.4 of docs/07-networking.md).
pub(crate) fn client_address_from_pb(
    field: &str,
    e: &pb::EndpointSpec,
    tls: Option<TlsSpec>,
) -> Result<Option<TlsSpec>> {
    match (e.client_address.as_known(), tls) {
        (Some(pb::ClientAddress::CLIENT_ADDRESS_UNSPECIFIED), tls) => Ok(tls),
        (Some(pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL), tls) => match tls {
            Some(TlsSpec::Passthrough { .. }) => {
                Ok(Some(TlsSpec::Passthrough { proxy_protocol: true }))
            }
            _ => Err(invalid(
                field,
                "the PROXY protocol is written by the proxy that passes TLS through: declared \
                 only with tls passthrough (§3.2 of docs/07-networking.md)",
            )),
        },
        _ => Err(invalid(
            field,
            "only proxy_protocol is declared; preserved and translated say what the platform \
             does (§3.4 of docs/07-networking.md)",
        )),
    }
}

/// The certificates `text` holds in PEM, each one `webpki` takes as a trust anchor, in DER.
fn authorities(field: &str, text: &str) -> Result<Vec<Vec<u8>>> {
    if text.len() > MAX_CLIENT_AUTHORITY_BYTES {
        let why = format!("at most {MAX_CLIENT_AUTHORITY_BYTES} bytes of PEM");
        return Err(invalid(field, why));
    }
    let mut out = Vec::new();
    for section in <(SectionKind, Vec<u8>)>::pem_slice_iter(text.as_bytes()) {
        let (kind, der) = section.map_err(|e| invalid(field, format!("not PEM: {e}")))?;
        if kind != SectionKind::Certificate {
            return Err(invalid(field, "certificates alone: an authority's key is not declared"));
        }
        webpki::anchor_from_trusted_cert(&CertificateDer::from(der.as_slice()))
            .map_err(|e| invalid(field, format!("not a certificate: {e}")))?;
        out.push(der);
    }
    // A section whose label is not one PEM names is skipped by the reader; it is refused here,
    // since whatever it is, it is not an authority's certificate.
    if text.matches("-----BEGIN ").count() != out.len() {
        return Err(invalid(field, "certificates alone, each in a CERTIFICATE section"));
    }
    Ok(out)
}

/// `authorities` in PEM, as a declaration gives them.
fn pem(authorities: &[Vec<u8>]) -> String {
    let mut out = String::new();
    for der in authorities {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let encoded = STANDARD.encode(der);
        for line in encoded.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).unwrap_or_default());
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}
