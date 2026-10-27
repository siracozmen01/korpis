// SPDX-License-Identifier: AGPL-3.0-or-later

//! HTTP/3 on UDP port 443 (§3.2 of docs/07-networking.md): QUIC with TLS 1.3, the certificate
//! chosen by the name the client asks for as on TCP, of a route that is terminated and asks for
//! no client certificate (`crate::tls`); each request whose authority is one of that route's names
//! forwarded to its workload over HTTP/1.1 as one over HTTP/2 is, and any other refused 421. A
//! WebSocket, which HTTP/3 would open by extended CONNECT, is not served here, and its client
//! keeps to TCP. Every connection counts against the proxy's limit as one over TCP does.

use super::Shared;
use crate::forward::{BoxError, Origin, answer};
use crate::table::normal;
use bytes::{Buf as _, Bytes};
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt as _;
use hyper::body::Frame;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// The streams one HTTP/3 connection carries at once, as an HTTP/2 one's (`super::MAX_STREAMS`).
const MAX_STREAMS: u32 = super::MAX_STREAMS;

/// A request's body as its HTTP/3 stream delivers it.
struct Received(mpsc::Receiver<Result<Bytes, BoxError>>);

impl hyper::body::Body for Received {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        self.0.poll_recv(cx).map(|r| r.map(|r| r.map(Frame::data)))
    }
}

/// The endpoint serving `socket`.
fn endpoint(socket: std::net::UdpSocket, shared: &Shared) -> io::Result<quinn::Endpoint> {
    let tls = crate::tls::quic_config(shared.table.clone()).map_err(io::Error::other)?;
    let crypto =
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(io::Error::other)?;
    let limits = shared.table().limits;
    let mut transport = quinn::TransportConfig::default();
    let idle = quinn::IdleTimeout::try_from(limits.idle_timeout).map_err(io::Error::other)?;
    transport.max_idle_timeout(Some(idle)).max_concurrent_bidi_streams(MAX_STREAMS.into());
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    server.transport_config(Arc::new(transport));
    let runtime = Arc::new(quinn::TokioRuntime);
    quinn::Endpoint::new(quinn::EndpointConfig::default(), Some(server), socket, runtime)
}

/// Serves HTTP/3 on `socket` until it fails.
pub(super) async fn serve(socket: std::net::UdpSocket, shared: Arc<Shared>) -> io::Result<()> {
    socket.set_nonblocking(true)?;
    let endpoint = endpoint(socket, &shared)?;
    while let Some(incoming) = endpoint.accept().await {
        let Some(counted) = shared.admit() else {
            incoming.refuse();
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if let Ok(connection) = incoming.await {
                connected(&shared, connection).await;
            }
            drop(counted);
        });
    }
    Ok(())
}

/// One connection: the route its handshake named, and each request on it.
async fn connected(shared: &Arc<Shared>, connection: quinn::Connection) {
    let client = connection.remote_address();
    let named = connection
        .handshake_data()
        .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|d| d.server_name);
    let route = named.and_then(|n| shared.table().route(&n).cloned());
    let Some(route) = route else { return };
    let limits = shared.table().limits;
    let field_section = u64::try_from(limits.max_header_bytes).unwrap_or(u64::MAX);
    let built = h3::server::builder()
        .max_field_section_size(field_section)
        .build::<_, Bytes>(h3_quinn::Connection::new(connection))
        .await;
    let Ok(mut h3) = built else { return };
    while let Ok(Some(resolver)) = h3.accept().await {
        let (shared, route) = (Arc::clone(shared), Arc::clone(&route));
        tokio::spawn(async move {
            if let Ok((req, stream)) = resolver.resolve_request().await {
                let origin = Origin { client, route, host: String::new(), presented: None };
                exchanged(&shared, origin, req, stream).await;
            }
        });
    }
}

type Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// Answers one request: forwarded where it names one of its connection's route's names.
async fn exchanged(shared: &Shared, connection: Origin, req: Request<()>, stream: Stream) {
    let (mut sending, receiving) = stream.split();
    let host = req.uri().authority().map(|a| normal(a.as_str()));
    let host = host.filter(|h| connection.route.names.contains(h));
    let response = match host {
        _ if req.method() == Method::CONNECT => {
            answer(StatusCode::NOT_IMPLEMENTED, "no tunnel over HTTP/3; use TCP\n")
        }
        None => answer(StatusCode::MISDIRECTED_REQUEST, "this connection serves other names\n"),
        Some(host) => {
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(receive(receiving, tx));
            let origin = Origin { host, ..connection };
            shared.backends.forward(&origin, req.map(|()| Received(rx))).await
        }
    };
    let _ = respond(&mut sending, response).await;
}

/// Passes the request's body from `stream` to `tx` as it comes, until it ends, fails, or the
/// request's forwarding stops reading it.
async fn receive(
    mut stream: h3::server::RequestStream<h3_quinn::RecvStream, Bytes>,
    tx: mpsc::Sender<Result<Bytes, BoxError>>,
) {
    loop {
        let next = match stream.recv_data().await {
            Ok(Some(mut b)) => Ok(b.copy_to_bytes(b.remaining())),
            Ok(None) => return,
            Err(e) => Err(BoxError::from(e)),
        };
        let failed = next.is_err();
        if tx.send(next).await.is_err() || failed {
            return;
        }
    }
}

/// Sends `response` down `stream`: its head, its body as it comes, and its trailers.
async fn respond(
    stream: &mut h3::server::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>,
    response: Response<crate::forward::Body>,
) -> Result<(), BoxError> {
    let (parts, mut body) = response.into_parts();
    stream.send_response(Response::from_parts(parts, ())).await?;
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        match frame.into_data() {
            Ok(data) => stream.send_data(data).await?,
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    stream.send_trailers(trailers).await?;
                }
            }
        }
    }
    stream.finish().await?;
    Ok(())
}
