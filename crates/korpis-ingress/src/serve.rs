// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serving the node's ingress sockets (§3.2 of docs/07-networking.md). On port 80 the proxy
//! answers ACME's HTTP challenges and redirects everything else it routes to HTTPS. On 443 it
//! reads the name the client asks for, then either splices the connection to a `passthrough`
//! workload unread or terminates TLS with the route's certificate and serves HTTP/1.1 and
//! HTTP/2, routing each request whose `Host` is one of the route's names, and a WebSocket opened
//! on either.

use crate::forward::{Backends, Body, Origin, answer};
use crate::headers::{wants_websocket, wants_websocket_h2};
use crate::sni::read_hello;
use crate::stream::{Idle, Replay};
use crate::table::{Limits, Mode, Route, Table, normal};
use http::{Request, Response, StatusCode, Version, header};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncWriteExt as _, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;

/// The sockets the proxy serves.
#[derive(Debug, Default)]
pub struct Listeners {
    /// Plain HTTP, port 80 of each address.
    pub http: Vec<std::net::TcpListener>,
    /// TLS, port 443 of each address.
    pub https: Vec<std::net::TcpListener>,
}

/// What every connection shares.
#[derive(Debug)]
struct Shared {
    table: watch::Receiver<Arc<Table>>,
    backends: Backends,
    tls: Arc<rustls::ServerConfig>,
    open: AtomicUsize,
}

/// One open connection, counted until it ends.
struct Counted(Arc<Shared>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Shared {
    fn table(&self) -> Arc<Table> {
        Arc::clone(&self.table.borrow())
    }

    /// Counts a new connection, unless as many are open as the limit allows.
    fn admit(self: &Arc<Self>) -> Option<Counted> {
        let limit = self.table().limits.max_connections;
        let open = self.open.fetch_add(1, Ordering::Relaxed);
        let counted = Counted(Arc::clone(self));
        (open < limit).then_some(counted)
    }
}

/// Serves `listeners` with what `table` holds at each moment, until a listener fails.
pub async fn serve(listeners: Listeners, table: watch::Receiver<Arc<Table>>) -> io::Result<()> {
    let tls = crate::tls::server_config(table.clone()).map_err(io::Error::other)?;
    let backends = Backends::new().map_err(io::Error::other)?;
    let shared = Arc::new(Shared { table, backends, tls, open: AtomicUsize::new(0) });
    let mut tasks = tokio::task::JoinSet::new();
    for (listener, secure) in listeners
        .http
        .into_iter()
        .map(|l| (l, false))
        .chain(listeners.https.into_iter().map(|l| (l, true)))
    {
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;
        tasks.spawn(accept(listener, Arc::clone(&shared), secure));
    }
    match tasks.join_next().await {
        Some(Ok(r)) => r,
        Some(Err(e)) => Err(io::Error::other(e)),
        None => Ok(()),
    }
}

async fn accept(listener: TcpListener, shared: Arc<Shared>, secure: bool) -> io::Result<()> {
    loop {
        let (stream, client) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let Some(counted) = shared.admit() else {
            tokio::spawn(refuse(stream, secure));
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if secure {
                secure_connection(&shared, stream, client).await;
            } else {
                plain_connection(&shared, stream).await;
            }
            drop(counted);
        });
    }
}

/// A connection beyond the limit: answered 503 where it speaks plain HTTP, and closed where it
/// would need a TLS handshake to be answered at all.
async fn refuse(mut stream: TcpStream, secure: bool) {
    if !secure {
        let busy =
            b"HTTP/1.1 503 Service Unavailable\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
        let _ = timeout(Duration::from_secs(1), stream.write_all(busy)).await;
    }
    let _ = stream.shutdown().await;
}

fn http1(limits: &Limits) -> hyper::server::conn::http1::Builder {
    let mut b = hyper::server::conn::http1::Builder::new();
    b.timer(TokioTimer::new())
        .header_read_timeout(limits.header_timeout)
        .max_buf_size(limits.max_header_bytes)
        .max_headers(limits.max_headers);
    b
}

async fn plain_connection(shared: &Arc<Shared>, stream: TcpStream) {
    let limits = shared.table().limits;
    let io = TokioIo::new(Idle::new(stream, limits.idle_timeout));
    let shared = Arc::clone(shared);
    let service = service_fn(move |req: Request<Incoming>| {
        let table = shared.table();
        async move { Ok::<_, Infallible>(plain(&table, &req)) }
    });
    let _ = http1(&limits).serve_connection(io, service).await;
}

/// Port 80: ACME's challenge, answered from the tokens the proxy was given; anything else for a
/// name it routes, redirected to HTTPS; anything for a name it does not, not found.
fn plain(table: &Table, req: &Request<Incoming>) -> Response<Body> {
    if let Some(token) = req.uri().path().strip_prefix("/.well-known/acme-challenge/") {
        return match table.challenge(token) {
            Some(key) => answer_owned(StatusCode::OK, key.to_owned()),
            None => answer(StatusCode::NOT_FOUND, "no such challenge\n"),
        };
    }
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(normal);
    let Some(host) = host.filter(|h| table.route(h).is_some()) else {
        return answer(StatusCode::NOT_FOUND, "no route for this name\n");
    };
    let target = req.uri().path_and_query().map_or("/", http::uri::PathAndQuery::as_str);
    let mut moved = answer(StatusCode::PERMANENT_REDIRECT, "moved to https\n");
    if let Ok(location) = http::HeaderValue::from_str(&format!("https://{host}{target}")) {
        moved.headers_mut().insert(header::LOCATION, location);
    }
    moved
}

fn answer_owned(status: StatusCode, text: String) -> Response<Body> {
    use http_body_util::BodyExt as _;
    let body = http_body_util::Full::new(bytes::Bytes::from(text)).map_err(|n| match n {});
    let mut r = Response::new(body.boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, http::HeaderValue::from_static("text/plain"));
    r
}

async fn secure_connection(shared: &Arc<Shared>, stream: TcpStream, client: SocketAddr) {
    let limits = shared.table().limits;
    let reached = stream.local_addr();
    let mut io = Idle::new(stream, limits.idle_timeout);
    let Ok(Ok(hello)) = timeout(limits.header_timeout, read_hello(&mut io)).await else { return };
    let route = hello.name.as_deref().and_then(|n| shared.table().route(n).cloned());
    let Some(route) = route else { return };
    if let Mode::Passthrough { proxy_protocol } = route.mode {
        let mut read = hello.read;
        if proxy_protocol {
            // The header names the address the client reached; without it there is no header,
            // and a workload that expects one is not sent the connection without it.
            let Ok(reached) = reached else { return };
            read.splice(0..0, crate::proxy_header::v2(client, reached));
        }
        return passthrough(io, read, route.backend).await;
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&shared.tls));
    let accepted = timeout(limits.header_timeout, acceptor.accept(Replay::new(hello.read, io)));
    let Ok(Ok(tls)) = accepted.await else { return };
    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_timeout)
        .max_buf_size(limits.max_header_bytes)
        .max_headers(limits.max_headers);
    let header_bytes = u32::try_from(limits.max_header_bytes).unwrap_or(u32::MAX);
    // Extended CONNECT (RFC 8441), so a WebSocket can open on an HTTP/2 connection.
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(header_bytes)
        .enable_connect_protocol();
    let shared = Arc::clone(shared);
    let service = service_fn(move |req| {
        let (shared, route) = (Arc::clone(&shared), Arc::clone(&route));
        async move { Ok::<_, Infallible>(request(&shared, route, client, req).await) }
    });
    let _ = builder.serve_connection_with_upgrades(TokioIo::new(tls), service).await;
}

/// One request on a terminated connection: routed when it names one of the names the
/// connection's route serves, and refused 421 otherwise, since the connection was made for
/// another name.
async fn request(
    shared: &Shared,
    route: Arc<Route>,
    client: SocketAddr,
    req: Request<Incoming>,
) -> Response<Body> {
    let named = req.uri().authority().map(|a| a.as_str().to_owned()).or_else(|| {
        req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_owned)
    });
    let host = named.map(|h| normal(&h)).filter(|h| route.names.contains(h));
    let Some(host) = host else {
        return answer(StatusCode::MISDIRECTED_REQUEST, "this connection serves other names\n");
    };
    let origin = Origin { client, route, host };
    if req.version() == Version::HTTP_11 && wants_websocket(req.headers()) {
        return shared.backends.websocket(&origin, req).await;
    }
    if req.version() == Version::HTTP_2 && wants_websocket_h2(&req) {
        return shared.backends.websocket_h2(&origin, req).await;
    }
    shared.backends.forward(&origin, req).await
}

/// Splices a client to a `passthrough` workload, first giving it the bytes read to route it,
/// after the PROXY header where its endpoint declared one.
async fn passthrough(mut client: Idle<TcpStream>, read: Vec<u8>, backend: SocketAddr) {
    let connected = timeout(Duration::from_secs(10), TcpStream::connect(backend)).await;
    let Ok(Ok(mut workload)) = connected else { return };
    if workload.write_all(&read).await.is_err() {
        return;
    }
    let _ = copy_bidirectional(&mut client, &mut workload).await;
}
