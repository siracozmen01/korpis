// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the proxy serves (§3.2 of docs/07-networking.md): the routes by name, the certificates
//! they are served with, the answers to ACME's HTTP challenges, and the bounds on what it accepts.
//! A configuration is taken whole or refused whole, so the proxy never serves half of one.

use korpis_proto::korpis::ingress::v1 as pb;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Why a configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// How a route's connections are served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// TLS terminated here; forwarded over HTTP/1.1.
    Http,
    /// TLS terminated here; forwarded over HTTP/1.1 within TLS, the workload's certificate not
    /// verified.
    Https,
    /// Spliced to the workload unread, routed by SNI.
    Passthrough {
        /// Whether the workload is first sent a PROXY header, version 2, naming its client.
        proxy_protocol: bool,
    },
}

/// The names one endpoint routes, and where its workload listens.
#[derive(Debug)]
pub struct Route {
    /// Its names, lowercase.
    pub names: Vec<String>,
    /// The workload's address and the endpoint's container port.
    pub backend: SocketAddr,
    /// How it is served.
    pub mode: Mode,
    /// The certificate it is served with; none while it has none, and it is then served on port
    /// 80 alone.
    pub certificate: Option<Arc<CertifiedKey>>,
    /// The workload and endpoint, for logs.
    pub label: String,
}

/// The bounds on what the proxy accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Connections open at once.
    pub max_connections: usize,
    /// The bytes of a request's head.
    pub max_header_bytes: usize,
    /// The header fields of a request.
    pub max_headers: usize,
    /// How long a client may take to send a request's head, or its TLS greeting.
    pub header_timeout: Duration,
    /// How long a connection may pass no byte.
    pub idle_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 10_000,
            max_header_bytes: 64 << 10,
            max_headers: 100,
            header_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(120),
        }
    }
}

impl Limits {
    fn from_pb(l: &pb::Limits) -> Self {
        let d = Self::default();
        let or = |v: u32, default: usize| if v == 0 { default } else { usize_of(v) };
        let ms = |v: u32, default: Duration| {
            if v == 0 { default } else { Duration::from_millis(u64::from(v)) }
        };
        Self {
            max_connections: or(l.max_connections, d.max_connections),
            // hyper reads a head in a buffer of at least 8 KiB, so no bound is set below it.
            max_header_bytes: or(l.max_header_bytes, d.max_header_bytes).max(8 << 10),
            max_headers: or(l.max_headers, d.max_headers),
            header_timeout: ms(l.header_timeout_ms, d.header_timeout),
            idle_timeout: ms(l.idle_timeout_ms, d.idle_timeout),
        }
    }
}

fn usize_of(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// Everything the proxy serves, as one value it swaps whole.
#[derive(Debug, Default)]
pub struct Table {
    routes: HashMap<String, Arc<Route>>,
    challenges: HashMap<String, String>,
    /// The bounds on what it accepts.
    pub limits: Limits,
}

/// A certificate and its key, from PEM, refused unless the key is the certificate's.
fn certified(c: &pb::Certificate) -> Result<CertifiedKey, ConfigError> {
    let bad = |what: &str| ConfigError(format!("certificate {}: {what}", c.id));
    let chain = CertificateDer::pem_slice_iter(c.chain.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| bad(&e.to_string()))?;
    if chain.is_empty() {
        return Err(bad("no certificate in its chain"));
    }
    let key = PrivateKeyDer::from_pem_slice(c.key.as_bytes()).map_err(|e| bad(&e.to_string()))?;
    let signer =
        rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| bad(&e.to_string()))?;
    let key = CertifiedKey::new(chain, signer);
    key.keys_match().map_err(|e| bad(&e.to_string()))?;
    Ok(key)
}

/// How `r` is served; none where it names no mode, or asks for a PROXY header on a connection
/// the proxy terminates, whose workload is told its client in `Forwarded`.
fn mode(r: &pb::Route) -> Option<Mode> {
    match r.mode.as_known()? {
        pb::RouteMode::ROUTE_MODE_HTTP if !r.proxy_protocol => Some(Mode::Http),
        pb::RouteMode::ROUTE_MODE_HTTPS if !r.proxy_protocol => Some(Mode::Https),
        pb::RouteMode::ROUTE_MODE_PASSTHROUGH => {
            Some(Mode::Passthrough { proxy_protocol: r.proxy_protocol })
        }
        _ => None,
    }
}

impl Table {
    /// The table `c` describes, or why it is refused.
    pub fn from_pb(c: &pb::IngressConfig) -> Result<Self, ConfigError> {
        let mut certificates = HashMap::new();
        for cert in &c.certificates {
            certificates.insert(cert.id.as_str(), Arc::new(certified(cert)?));
        }
        let mut routes = HashMap::new();
        for r in &c.routes {
            let label = format!("{}/{}", r.workload, r.endpoint);
            let bad = |what: &str| ConfigError(format!("route {label}: {what}"));
            let backend = r.backend.parse().map_err(|_| bad("a backend is ip:port"))?;
            let mode = mode(r).ok_or_else(|| bad("no mode, or a PROXY header it cannot carry"))?;
            let certificate = match r.certificate.as_str() {
                "" => None,
                id => Some(Arc::clone(
                    certificates.get(id).ok_or_else(|| bad("no such certificate"))?,
                )),
            };
            let names: Vec<String> = r.names.iter().map(|n| n.to_ascii_lowercase()).collect();
            let route = Arc::new(Route {
                names: names.clone(),
                backend,
                mode,
                certificate,
                label: label.clone(),
            });
            if let Some(twice) =
                names.into_iter().find(|n| routes.insert(n.clone(), Arc::clone(&route)).is_some())
            {
                return Err(bad(&format!("{twice} is routed twice")));
            }
        }
        let challenges = c
            .challenges
            .iter()
            .map(|ch| (ch.token.clone(), ch.key_authorization.clone()))
            .collect();
        let limits = Limits::from_pb(c.limits.as_option().unwrap_or(&pb::Limits::default()));
        Ok(Self { routes, challenges, limits })
    }

    /// The route of `name`, as a client named it in SNI or `Host`: without a port or a final
    /// dot, in any case.
    #[must_use]
    pub fn route(&self, name: &str) -> Option<&Arc<Route>> {
        self.routes.get(&normal(name))
    }

    /// The answer to the ACME challenge `token`.
    #[must_use]
    pub fn challenge(&self, token: &str) -> Option<&str> {
        self.challenges.get(token).map(String::as_str)
    }
}

/// `host` without a port or a final dot, lowercase. A colon is a port's only after a name or a
/// bracketed IPv6 literal.
#[must_use]
pub fn normal(host: &str) -> String {
    let bare = match host.rsplit_once(':') {
        Some((h, port))
            if !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && (!h.contains(':') || h.ends_with(']')) =>
        {
            h
        }
        _ => host,
    };
    bare.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests;
