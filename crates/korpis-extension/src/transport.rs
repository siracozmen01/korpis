// SPDX-License-Identifier: Apache-2.0

//! How the extension reaches the control plane: directly, over the network, or through the
//! relay its node serves in its directory (§9.5 of docs/08-identity.md). Either way it speaks TLS
//! to the control plane itself and verifies it under the CA the identity socket gave, so the
//! relay carries what it cannot read.

use crate::error::Error;
use connectrpc::ConnectError;
use connectrpc::client::{BoxFuture, ClientBody, ClientTransport, Http2Connection};
use connectrpc::client::{HttpClient, SharedHttp2Connection};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many calls may wait for the relayed connection at once.
const WAITING: usize = 1024;

/// A transport for a generated client.
#[derive(Clone)]
pub enum Transport {
    /// Over the network, to the control plane's address.
    Direct(Box<HttpClient>),
    /// Through the node's relay, one HTTP/2 connection carried over it.
    Relayed(SharedHttp2Connection),
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Direct(_) => "Transport::Direct",
            Self::Relayed(_) => "Transport::Relayed",
        })
    }
}

impl ClientTransport for Transport {
    type ResponseBody = <HttpClient as ClientTransport>::ResponseBody;
    type Error = ConnectError;

    fn send(
        &self,
        request: http::Request<ClientBody>,
    ) -> BoxFuture<'static, Result<http::Response<Self::ResponseBody>, Self::Error>> {
        match self {
            Self::Direct(h) => h.as_ref().send(request),
            Self::Relayed(c) => c.send(request),
        }
    }
}

fn local(what: &str, e: &dyn std::fmt::Display) -> Error {
    Error::Local(format!("{what}: {e}"))
}

/// TLS that trusts `ca_pem` alone.
fn tls(ca_pem: &[u8]) -> Result<rustls::ClientConfig, Error> {
    let ca = CertificateDer::from_pem_slice(ca_pem).map_err(|e| local("the CA", &e))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).map_err(|e| local("the CA", &e))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| local("TLS", &e))?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// The transport to the control plane at `address` under `ca_pem`: through `relay` where the
/// node serves one, and over the network otherwise. For a client that keeps its own bearer, such
/// as the `korpis` command run inside a workload; [`crate::Connection`] does both.
///
/// # Errors
/// The CA or the address is malformed.
pub fn transport(address: &str, ca_pem: &[u8], relay: Option<&Path>) -> Result<Transport, Error> {
    let config = tls(ca_pem)?;
    let Some(relay) = relay else {
        return Ok(Transport::Direct(Box::new(HttpClient::with_tls(Arc::new(config)))));
    };
    let uri: http::Uri = address.parse().map_err(|e| local("the address", &e))?;
    let host = uri.host().ok_or_else(|| Error::Local("the address names no host".into()))?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
    let name = ServerName::try_from(host).map_err(|e| local("the address", &e))?;
    Ok(Transport::Relayed(relayed(relay.to_owned(), config, name, uri)))
}

/// One HTTP/2 connection to the control plane over TLS over the relay socket at `path`, made
/// again whenever it drops.
fn relayed(
    path: PathBuf,
    mut config: rustls::ClientConfig,
    name: ServerName<'static>,
    uri: http::Uri,
) -> SharedHttp2Connection {
    config.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let connect = tower::service_fn(move |_: http::Uri| {
        let (path, connector, name) = (path.clone(), connector.clone(), name.clone());
        async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            let tls = connector.connect(name, stream).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(tls))
        }
    });
    Http2Connection::lazy_with_connector(connect, uri).shared(WAITING)
}
