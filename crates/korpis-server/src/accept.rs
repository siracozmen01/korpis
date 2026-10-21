// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serving behind a load balancer (§3.1 of docs/02-architecture.md). A connection from one of the
//! configured proxy addresses must open with a PROXY header, and the client it names is the
//! peer every handler sees, as `source_cidr` is judged by; a connection from anywhere else is its
//! own client, and a header it sends is not read as one, because a header anyone can send is not
//! an address. connectrpc's own server reads no header and serves nothing but the API, so every
//! connection is accepted here and served by the same hyper, with bounds on a slow handshake,
//! through the front that answers the panel's files beside the API (`crate::front`).
//!
//! The `ClientHello` is read before a configuration is chosen (§3.6 of docs/07-networking.md): a
//! portal's name is served with its portal's certificate and the request told which portal it is
//! for, a name offering only `acme-tls/1` is answered with its order's challenge and closed, and
//! any other name, or none, is served with the installation's own certificate, as before.

pub mod proxy;

use crate::front::Front;
use crate::portals::{ACME_TLS, Named, PortalAt, Portals};
use connectrpc::server::PeerAddr;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use ipnet::IpNet;
use proxy::{Header, read_header};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::LazyConfigAcceptor;
use tokio_rustls::server::TlsStream;
use tower_service::Service as _;

/// How long a connection has for its PROXY header, and again for its TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a connection has to send its first request's headers.
const HEADERS: Duration = Duration::from_secs(30);

/// How a listener's connections are given their TLS: the installation's own configuration, and
/// the portals it serves, if it serves any.
#[derive(Debug, Clone)]
pub struct Tls {
    /// The installation's own.
    pub own: Arc<rustls::ServerConfig>,
    /// The portals, chosen by SNI.
    pub portals: Option<Arc<Portals>>,
}

impl Tls {
    /// The installation's own configuration alone.
    #[must_use]
    pub const fn own(own: Arc<rustls::ServerConfig>) -> Self {
        Self { own, portals: None }
    }
}

/// Serves `service` on `listener` with `tls`, as [`serve`] does. Ends only on failure.
pub async fn serve_on(
    listener: TcpListener,
    tls: Tls,
    service: Front,
    proxies: Arc<[IpNet]>,
) -> Result<(), String> {
    serve(listener, tls, service, proxies).await.map_err(|e| e.to_string())
}

/// Serves `service` on `listener` with `tls`, reading a PROXY header from every connection whose
/// peer is in `proxies`. Ends only if accepting fails.
pub async fn serve(
    listener: TcpListener,
    tls: Tls,
    service: Front,
    proxies: Arc<[IpNet]>,
) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let (tls, service, proxies) = (tls.clone(), service.clone(), Arc::clone(&proxies));
        tokio::spawn(async move {
            if let Err(e) = connection(stream, peer, (&tls, service), &proxies).await {
                tracing::debug!(%peer, error = %e, "connection refused");
            }
        });
    }
}

/// The client `stream` from `peer` was accepted for: the one its PROXY header names when `peer`
/// is a configured proxy, `peer` itself otherwise.
async fn client(
    stream: &mut TcpStream,
    peer: SocketAddr,
    proxies: &[IpNet],
) -> Result<SocketAddr, String> {
    if !proxies.iter().any(|p| p.contains(&peer.ip())) {
        return Ok(peer);
    }
    let header = tokio::time::timeout(HANDSHAKE, read_header(stream))
        .await
        .map_err(|_| "no PROXY header in time".to_owned())?
        .map_err(|e| e.to_string())?;
    Ok(match header {
        Header::Client(client) => client,
        Header::Local => peer,
    })
}

/// What a `ClientHello` asks for: the name it sent in SNI, lowercase, and whether `acme-tls/1` is
/// the only protocol it offers.
fn asked(hello: &rustls::server::ClientHello<'_>) -> (Option<String>, bool) {
    let name = hello.server_name().map(str::to_ascii_lowercase);
    let protocols: Vec<&[u8]> = hello.alpn().map(Iterator::collect).unwrap_or_default();
    let acme = !protocols.is_empty() && protocols.iter().all(|p| *p == ACME_TLS);
    (name, acme)
}

/// The TLS of `stream` once its `ClientHello` is read: the configuration its name asks for, and
/// the portal it is for, if any; none once a TLS-ALPN-01 validation is answered, or refused.
async fn handshake(
    stream: TcpStream,
    tls: &Tls,
) -> Result<Option<(TlsStream<TcpStream>, Option<PortalAt>)>, String> {
    let acceptor = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream);
    let start = acceptor.await.map_err(|e| e.to_string())?;
    let (name, acme) = asked(&start.client_hello());
    let named = match (name.as_deref(), tls.portals.as_ref()) {
        (Some(n), Some(p)) => p.named(n),
        _ => Named::Other,
    };
    if acme {
        let portals = tls.portals.as_ref().ok_or("no portal is served here")?;
        let name = name.ok_or("a validation names its name")?;
        let config = portals.challenge(&name).await.ok_or("no challenge for this name")?;
        // The validation reads the certificate in the handshake; nothing is served after it.
        start.into_stream(config).await.map_err(|e| e.to_string())?;
        return Ok(None);
    }
    let (config, at) = match named {
        Named::Portal(s) => (Arc::clone(&s.config), Some(s.at.clone())),
        Named::Unserved => return Err("the portal's certificate is not issued".into()),
        Named::Other => (Arc::clone(&tls.own), None),
    };
    let stream = start.into_stream(config).await.map_err(|e| e.to_string())?;
    Ok(Some((stream, at)))
}

async fn connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    (tls, service): (&Tls, Front),
    proxies: &[IpNet],
) -> Result<(), String> {
    let client = client(&mut stream, peer, proxies).await?;
    let shaken = tokio::time::timeout(HANDSHAKE, handshake(stream, tls))
        .await
        .map_err(|_| "no TLS handshake in time".to_owned())??;
    let Some((tls, at)) = shaken else { return Ok(()) };
    let serve = hyper::service::service_fn(move |mut request: http::Request<_>| {
        request.extensions_mut().insert(PeerAddr(client));
        if let Some(at) = &at {
            request.extensions_mut().insert(at.clone());
        }
        service.clone().call(request)
    });
    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(HEADERS);
    builder.serve_connection(TokioIo::new(tls), serve).await.map_err(|e| e.to_string())
}
