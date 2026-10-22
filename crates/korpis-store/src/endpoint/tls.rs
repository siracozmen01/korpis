// SPDX-License-Identifier: AGPL-3.0-or-later

//! An endpoint's TLS as stored (migrations 0056, 0118 and 0119): where its certificate comes
//! from and which, whether its workload is sent a PROXY header, and the client certificates it
//! asks for with the authorities it trusts.

use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::workload::spec::endpoints::{ClientAuth, ClientCertificates, TlsSpec};

/// The columns `tls` is stored in.
pub(super) struct Columns<'a> {
    pub(super) source: Option<&'static str>,
    pub(super) secret: &'a str,
    pub(super) proxy_protocol: bool,
    pub(super) client_certificates: Option<&'static str>,
    pub(super) client_ca: Vec<Vec<u8>>,
}

const CERTIFICATES: [(ClientCertificates, &str); 2] =
    [(ClientCertificates::Required, "required"), (ClientCertificates::Optional, "optional")];

fn clients(c: Option<&ClientAuth>) -> (Option<&'static str>, Vec<Vec<u8>>) {
    c.map_or((None, Vec::new()), |c| {
        (Some(super::named(&CERTIFICATES, c.certificates)), c.authorities.clone())
    })
}

/// The stored form of `t`.
pub(super) fn columns(t: Option<&TlsSpec>) -> Columns<'_> {
    let (source, secret, proxy_protocol, asked) = match t {
        None => (None, "", false, None),
        Some(TlsSpec::Acme { clients }) => (Some("acme"), "", false, clients.as_ref()),
        Some(TlsSpec::Provided { secret, clients }) => {
            (Some("provided"), secret.as_str(), false, clients.as_ref())
        }
        Some(TlsSpec::Passthrough { proxy_protocol }) => {
            (Some("passthrough"), "", *proxy_protocol, None)
        }
    };
    let (client_certificates, client_ca) = clients(asked);
    Columns { source, secret, proxy_protocol, client_certificates, client_ca }
}

/// What the columns stored hold.
pub(super) struct Stored {
    pub(super) source: Option<String>,
    pub(super) secret: String,
    pub(super) proxy_protocol: bool,
    pub(super) client_certificates: Option<String>,
    pub(super) client_ca: Vec<Vec<u8>>,
}

/// The TLS `s` holds.
pub(super) fn tls(s: Stored) -> StoreResult<Option<TlsSpec>> {
    let clients = match s.client_certificates.as_deref() {
        None => None,
        Some(c) => Some(ClientAuth {
            certificates: super::parsed(&CERTIFICATES, "client certificates", c)?,
            authorities: s.client_ca,
        }),
    };
    Ok(match s.source.as_deref() {
        None => None,
        Some("acme") => Some(TlsSpec::Acme { clients }),
        Some("provided") => Some(TlsSpec::Provided { secret: s.secret, clients }),
        Some("passthrough") => Some(TlsSpec::Passthrough { proxy_protocol: s.proxy_protocol }),
        Some(other) => return Err(StoreError::Corrupt(format!("endpoint tls {other:?}"))),
    })
}
