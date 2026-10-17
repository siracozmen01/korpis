// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serving behind a load balancer (§3.1 of docs/02-architecture.md). A connection from one of the
//! configured proxy addresses must open with a PROXY header, and the client it names is the
//! peer every handler sees, as `source_cidr` is judged by; a connection from anywhere else is its
//! own client, and a header it sends is not read as one, because a header anyone can send is not
//! an address. connectrpc's own server reads no header and serves nothing but the API, so every
//! connection is accepted here and served by the same hyper, with bounds on a slow handshake,
//! through the front that answers the panel's files beside the API (`crate::front`).

pub mod proxy;

use crate::front::Front;
use connectrpc::server::PeerAddr;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use ipnet::IpNet;
use proxy::{Header, read_header};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tower_service::Service as _;

/// How long a connection has for its PROXY header, and again for its TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a connection has to send its first request's headers.
const HEADERS: Duration = Duration::from_secs(30);

/// Serves `service` on `listener` with `tls`, as [`serve`] does. Ends only on failure.
pub async fn serve_on(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    service: Front,
    proxies: Arc<[IpNet]>,
) -> Result<(), String> {
    serve(listener, tls, service, proxies).await.map_err(|e| e.to_string())
}

/// Serves `service` on `listener` with `tls`, reading a PROXY header from every connection whose
/// peer is in `proxies`. Ends only if accepting fails.
pub async fn serve(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    service: Front,
    proxies: Arc<[IpNet]>,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(tls);
    loop {
        let (stream, peer) = listener.accept().await?;
        let (acceptor, service, proxies) =
            (acceptor.clone(), service.clone(), Arc::clone(&proxies));
        tokio::spawn(async move {
            if let Err(e) = connection(stream, peer, (&acceptor, service), &proxies).await {
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

async fn connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    (acceptor, service): (&TlsAcceptor, Front),
    proxies: &[IpNet],
) -> Result<(), String> {
    let client = client(&mut stream, peer, proxies).await?;
    let tls = tokio::time::timeout(HANDSHAKE, acceptor.accept(stream))
        .await
        .map_err(|_| "no TLS handshake in time".to_owned())?
        .map_err(|e| e.to_string())?;
    let serve = hyper::service::service_fn(move |mut request: http::Request<_>| {
        request.extensions_mut().insert(PeerAddr(client));
        service.clone().call(request)
    });
    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(HEADERS);
    builder.serve_connection(TokioIo::new(tls), serve).await.map_err(|e| e.to_string())
}
