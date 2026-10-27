// SPDX-License-Identifier: AGPL-3.0-or-later

//! Forwarding a request to its workload over HTTP/1.1 (§3.2 of docs/07-networking.md), within
//! TLS for a workload that serves its own certificate. Connections to workloads are pooled. A
//! WebSocket upgrade gets a connection of its own, and once the workload agrees the two
//! connections are joined byte for byte; one opened on HTTP/2 by extended CONNECT (RFC 8441) is
//! forwarded as the same HTTP/1.1 upgrade, and its stream joined to the workload's connection.

use crate::headers::{set_client_cert, set_forwarded, strip_hop_by_hop};
use crate::table::{Mode, Route};
use bytes::Bytes;
use http::header::{self, HeaderValue};
use http::{Request, Response, StatusCode, Uri, Version};
use http_body_util::combinators::{BoxBody, UnsyncBoxBody};
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

/// A response body: a workload's, or one the proxy makes.
pub type Body = BoxBody<Bytes, hyper::Error>;

/// What failed reading a request's body, whatever protocol the client spoke.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A request body as it is sent to a workload: hyper's, from HTTP/1.1 and HTTP/2, or one an
/// HTTP/3 stream delivers.
type Sent = UnsyncBoxBody<Bytes, BoxError>;

/// A response the proxy makes itself, of `status` with `text`.
#[must_use]
pub fn answer(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut r =
        Response::new(Full::new(Bytes::from_static(text.as_bytes())).map_err(never).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    r
}

fn never(n: std::convert::Infallible) -> hyper::Error {
    match n {}
}

/// Where a request is forwarded from: its client, the route it reached, the name it asked for.
#[derive(Debug)]
pub struct Origin {
    /// The client's address.
    pub client: SocketAddr,
    /// The route its connection reached.
    pub route: Arc<Route>,
    /// The name it asked for, without a port.
    pub host: String,
    /// The certificate chain the client presented and the handshake verified, end-entity first;
    /// none where it presented none.
    pub presented: Option<Arc<[CertificateDer<'static>]>>,
}

/// The client connections to workloads are made with: pooled, HTTP/1.1, over TLS that verifies
/// nothing for a workload that serves its own certificate.
#[derive(Debug, Clone)]
pub struct Backends {
    pooled: Client<HttpsConnector<HttpConnector>, Sent>,
    tls: Arc<rustls::ClientConfig>,
}

impl Backends {
    /// A client for every workload of the node.
    pub fn new() -> Result<Self, rustls::Error> {
        let tls = Arc::new(crate::unverified::client_config()?);
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(Some(Duration::from_secs(10)));
        let connector = HttpsConnectorBuilder::new()
            .with_tls_config((*tls).clone())
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        let pooled = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .build(connector);
        Ok(Self { pooled, tls })
    }

    /// Forwards `req` from `origin` to its workload and answers with the workload's response, or
    /// 502 when the workload cannot be reached.
    pub async fn forward<B>(&self, origin: &Origin, req: Request<B>) -> Response<Body>
    where
        B: hyper::body::Body<Data = Bytes> + Send + 'static,
        B::Error: Into<BoxError>,
    {
        let (mut parts, body) = req.into_parts();
        let body: Sent = body.map_err(Into::into).boxed_unsync();
        let Some(uri) = backend_uri(&origin.route, parts.uri.path_and_query()) else {
            return answer(StatusCode::BAD_REQUEST, "a request target the proxy cannot forward\n");
        };
        parts.uri = uri;
        parts.version = Version::HTTP_11;
        prepare(&mut parts.headers, origin, false);
        match self.pooled.request(Request::from_parts(parts, body)).await {
            Ok(response) => {
                let (mut parts, body) = response.into_parts();
                strip_hop_by_hop(&mut parts.headers, false);
                Response::from_parts(parts, body.boxed())
            }
            Err(e) => {
                tracing::warn!(route = %origin.route.label, error = %e, "the workload did not answer");
                answer(StatusCode::BAD_GATEWAY, "the workload did not answer\n")
            }
        }
    }

    /// Forwards a WebSocket upgrade on a connection of its own, and joins the client's connection
    /// to the workload's once the workload switches protocols.
    pub async fn websocket(&self, origin: &Origin, mut req: Request<Incoming>) -> Response<Body> {
        let from_client = hyper::upgrade::on(&mut req);
        let (mut parts, body) = req.into_parts();
        parts.uri = parts.uri.path_and_query().map_or_else(
            || Uri::from_static("/"),
            |pq| Uri::try_from(pq.as_str()).unwrap_or_else(|_| Uri::from_static("/")),
        );
        prepare(&mut parts.headers, origin, true);
        parts.headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        let mut response = match self.upgrade(&origin.route, Request::from_parts(parts, body)).await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(route = %origin.route.label, error = %e, "no WebSocket to the workload");
                return answer(StatusCode::BAD_GATEWAY, "the workload did not answer\n");
            }
        };
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let from_workload = hyper::upgrade::on(&mut response);
            tokio::spawn(join(from_client, from_workload));
        }
        let (parts, body) = response.into_parts();
        Response::from_parts(parts, body.boxed())
    }

    /// Forwards a WebSocket opened on HTTP/2 by extended CONNECT (RFC 8441) as the HTTP/1.1
    /// upgrade the workload understands, with a key the proxy makes, since an HTTP/2 client sends
    /// none; once the workload switches protocols the client is answered 200, HTTP/2 having no
    /// 101, and the stream is joined to the workload's connection, `counted` held until the two
    /// part. Any other answer is returned.
    pub async fn websocket_h2<C: Send + 'static>(
        &self,
        origin: &Origin,
        mut req: Request<Incoming>,
        counted: C,
    ) -> Response<Body> {
        let from_client = hyper::upgrade::on(&mut req);
        let (mut parts, _) = req.into_parts();
        let mut asked = Request::new(Empty::<Bytes>::new());
        *asked.uri_mut() = parts.uri.path_and_query().map_or_else(
            || Uri::from_static("/"),
            |pq| Uri::try_from(pq.as_str()).unwrap_or_else(|_| Uri::from_static("/")),
        );
        prepare(&mut parts.headers, origin, true);
        let h = &mut parts.headers;
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        if let Some(key) = websocket_key() {
            h.insert(header::SEC_WEBSOCKET_KEY, key);
        }
        h.entry(header::SEC_WEBSOCKET_VERSION).or_insert(HeaderValue::from_static("13"));
        *asked.headers_mut() = parts.headers;
        let mut response = match self.upgrade(&origin.route, asked).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(route = %origin.route.label, error = %e, "no WebSocket to the workload");
                return answer(StatusCode::BAD_GATEWAY, "the workload did not answer\n");
            }
        };
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            let (mut parts, body) = response.into_parts();
            strip_hop_by_hop(&mut parts.headers, false);
            return Response::from_parts(parts, body.boxed());
        }
        let from_workload = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            join(from_client, from_workload).await;
            drop(counted);
        });
        let mut opened = Response::new(empty());
        for name in [header::SEC_WEBSOCKET_PROTOCOL, header::SEC_WEBSOCKET_EXTENSIONS] {
            if let Some(v) = response.headers().get(&name) {
                opened.headers_mut().insert(name, v.clone());
            }
        }
        opened
    }

    /// Sends `req` on a new HTTP/1.1 connection to `route`'s workload, kept for an upgrade.
    async fn upgrade<B>(
        &self,
        route: &Route,
        req: Request<B>,
    ) -> Result<Response<Incoming>, Box<dyn std::error::Error + Send + Sync>>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let tcp = TcpStream::connect(route.backend).await?;
        tcp.set_nodelay(true)?;
        if route.mode == Mode::Https {
            let name = rustls::pki_types::ServerName::IpAddress(route.backend.ip().into());
            let tls = tokio_rustls::TlsConnector::from(Arc::clone(&self.tls));
            let io = TokioIo::new(tls.connect(name, tcp).await?);
            send(io, req).await
        } else {
            send(TokioIo::new(tcp), req).await
        }
    }
}

/// Joins a client's upgraded connection to its workload's, byte for byte, until either ends.
async fn join(client: hyper::upgrade::OnUpgrade, workload: hyper::upgrade::OnUpgrade) {
    let (Ok(client), Ok(workload)) = tokio::join!(client, workload) else { return };
    let (mut client, mut workload) = (TokioIo::new(client), TokioIo::new(workload));
    let _ = tokio::io::copy_bidirectional(&mut client, &mut workload).await;
}

async fn send<I, B>(
    io: I,
    req: Request<B>,
) -> Result<Response<Incoming>, Box<dyn std::error::Error + Send + Sync>>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    Ok(sender.send_request(req).await?)
}

/// A fresh `Sec-WebSocket-Key`: sixteen random bytes in base64 (RFC 6455 §4.1).
fn websocket_key() -> Option<HeaderValue> {
    use base64::Engine as _;
    let mut nonce = [0u8; 16];
    crate::unverified::provider().secure_random.fill(&mut nonce).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(nonce);
    HeaderValue::from_str(&encoded).ok()
}

/// The headers a request leaves with: its own, less what describes its hop and what says where
/// it came from, with the proxy's account of that, and the name the client asked for.
fn prepare(h: &mut http::HeaderMap, origin: &Origin, upgrading: bool) {
    strip_hop_by_hop(h, upgrading);
    set_forwarded(h, origin.client, &origin.host, "https");
    set_client_cert(h, origin.presented.as_deref());
    if let Ok(host) = HeaderValue::from_str(&origin.host) {
        h.insert(header::HOST, host);
    }
}

/// The request target at `route`'s workload.
fn backend_uri(route: &Route, pq: Option<&http::uri::PathAndQuery>) -> Option<Uri> {
    let scheme = if route.mode == Mode::Https { "https" } else { "http" };
    let pq = pq.map_or("/", http::uri::PathAndQuery::as_str);
    format!("{scheme}://{}{pq}", route.backend).parse().ok()
}

/// An empty body, for a response made here.
#[must_use]
pub fn empty() -> Body {
    Empty::new().map_err(never).boxed()
}
