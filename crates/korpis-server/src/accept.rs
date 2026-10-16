// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serving behind a load balancer (§3.1 of docs/02-architecture.md). A connection from one of the
//! configured proxy addresses must open with a PROXY header, and the client it names is the
//! peer every handler sees, as `source_cidr` is judged by; a connection from anywhere else is its
//! own client, and a header it sends is not read as one, because a header anyone can send is not
//! an address. connectrpc's own server reads no header, so these connections are accepted here
//! and served by the same hyper, with the same bounds on a slow handshake.

pub mod proxy;

use connectrpc::ConnectRpcService;
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

/// Serves `service` on `listener` with `tls`: with connectrpc's own server when no proxy is
/// configured, and with [`serve`] when one is. Ends only on failure.
pub async fn serve_on(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    service: ConnectRpcService,
    proxies: Arc<[IpNet]>,
) -> Result<(), String> {
    if proxies.is_empty() {
        let bound = connectrpc::server::Server::from_listener(listener).with_tls(tls);
        bound.serve_with_service(service).await.map_err(|e| e.to_string())
    } else {
        serve(listener, tls, service, proxies).await.map_err(|e| e.to_string())
    }
}

/// Serves `service` on `listener` with `tls`, reading a PROXY header from every connection whose
/// peer is in `proxies`. Ends only if accepting fails.
pub async fn serve(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    service: ConnectRpcService,
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
    (acceptor, service): (&TlsAcceptor, ConnectRpcService),
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
