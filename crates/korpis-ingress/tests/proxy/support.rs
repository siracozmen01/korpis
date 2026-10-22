// SPDX-License-Identifier: AGPL-3.0-or-later

//! A certificate authority, workloads that say what they were sent, the proxy in front of them,
//! and clients that reach it the way the internet would.

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use korpis_ingress::{Listeners, Table};
use korpis_proto::korpis::ingress::v1 as pb;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// What a client presents: its certificate chain, end-entity first, and its key.
pub type Identity = (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>);

/// A certificate authority the test clients trust.
pub struct Pki {
    ca: rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
}

impl Pki {
    pub fn new() -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        Self { ca: rcgen::CertifiedIssuer::self_signed(params, key).unwrap() }
    }

    /// A certificate for `names` and its key, in PEM.
    pub fn leaf(&self, names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let params = rcgen::CertificateParams::new(names).unwrap();
        let cert = params.signed_by(&key, &self.ca).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    pub fn certificate(&self, id: &str, names: &[&str]) -> pb::Certificate {
        let (chain, key) = self.leaf(names);
        pb::Certificate { id: id.into(), chain, key, ..Default::default() }
    }

    /// The authority's own certificate, in PEM, as an endpoint trusting it declares it.
    pub fn authority(&self) -> String {
        self.ca.pem()
    }

    /// A client's certificate for `name`, its chain, end-entity first, and its key.
    pub fn client(&self, name: &str) -> Identity {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let cert = params.signed_by(&key, &self.ca).unwrap();
        let key = PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap();
        (vec![cert.der().clone()], key)
    }

    fn roots(&self) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.ca.der().clone()).unwrap();
        roots
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A server configuration presenting `chain` and `key`.
pub fn server_tls((chain, key): &(String, String)) -> Arc<rustls::ServerConfig> {
    let chain = CertificateDer::pem_slice_iter(chain.as_bytes()).collect::<Result<_, _>>();
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain.unwrap(), key)
        .unwrap();
    Arc::new(config)
}

/// What a workload answers: the request as it arrived, one `name=value` line each.
fn said(req: &Request<Incoming>, workload: &str) -> String {
    let get = |name: &str| {
        let all: Vec<&str> =
            req.headers().get_all(name).iter().filter_map(|v| v.to_str().ok()).collect();
        all.join(",")
    };
    format!(
        "workload={workload}\npath={}\nhost={}\nforwarded={}\nx-forwarded-for={}\nx-forwarded-proto={}\nversion={:?}\nclient-cert={}\nclient-cert-chain={}\n",
        req.uri(),
        get("host"),
        get("forwarded"),
        get("x-forwarded-for"),
        get("x-forwarded-proto"),
        req.version(),
        get("client-cert"),
        get("client-cert-chain")
    )
}

/// Echoes what arrives on an upgraded connection.
async fn echo(upgrade: hyper::upgrade::OnUpgrade) {
    let Ok(upgraded) = upgrade.await else { return };
    let mut io = TokioIo::new(upgraded);
    let mut buf = [0u8; 64];
    while let Ok(n @ 1..) = io.read(&mut buf).await {
        let Ok(()) = io.write_all(&buf[..n]).await else { return };
    }
}

/// One request at a workload: a WebSocket upgrade it accepts and then echoes, or anything else
/// answered with what it was sent.
fn answer(mut req: Request<Incoming>, workload: &'static str) -> Response<Full<Bytes>> {
    if req.headers().get(header::UPGRADE).is_some() {
        tokio::spawn(echo(hyper::upgrade::on(&mut req)));
        let mut r = Response::new(Full::new(Bytes::new()));
        *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        r.headers_mut().insert(header::UPGRADE, http::HeaderValue::from_static("websocket"));
        r.headers_mut().insert(header::CONNECTION, http::HeaderValue::from_static("upgrade"));
        return r;
    }
    Response::new(Full::new(Bytes::from(said(&req, workload))))
}

/// Serves one connection at workload `name`, behind `tls` if it has its own.
pub async fn serve_one(
    stream: TcpStream,
    tls: Option<Arc<rustls::ServerConfig>>,
    name: &'static str,
) {
    let service = service_fn(move |req| std::future::ready(Ok::<_, Infallible>(answer(req, name))));
    let http = hyper::server::conn::http1::Builder::new();
    let Some(config) = tls else {
        let _ = http.serve_connection(TokioIo::new(stream), service).with_upgrades().await;
        return;
    };
    let Ok(s) = tokio_rustls::TlsAcceptor::from(config).accept(stream).await else { return };
    let _ = http.serve_connection(TokioIo::new(s), service).with_upgrades().await;
}

/// A workload on loopback, plain or behind its own TLS; its address.
pub async fn workload(name: &'static str, tls: Option<Arc<rustls::ServerConfig>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_one(stream, tls.clone(), name));
        }
    });
    at
}

/// The proxy, on loopback, serving what the test publishes.
pub struct Proxy {
    pub http: SocketAddr,
    pub https: SocketAddr,
    publish: watch::Sender<Arc<Table>>,
}

impl Proxy {
    pub fn start(config: &pb::IngressConfig) -> Self {
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (h, s) = (http.local_addr().unwrap(), https.local_addr().unwrap());
        let (publish, table) = watch::channel(Arc::new(Table::from_pb(config).unwrap()));
        let listeners = Listeners { http: vec![http], https: vec![https] };
        tokio::spawn(korpis_ingress::serve(listeners, table));
        Self { http: h, https: s, publish }
    }

    pub fn set(&self, config: &pb::IngressConfig) {
        self.publish.send_replace(Arc::new(Table::from_pb(config).unwrap()));
    }
}

pub fn route(names: &[&str], backend: SocketAddr, mode: pb::RouteMode, cert: &str) -> pb::Route {
    pb::Route {
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        backend: backend.to_string(),
        mode: mode.into(),
        certificate: cert.into(),
        workload: "wl".into(),
        endpoint: "web".into(),
        ..Default::default()
    }
}

/// A TLS connection to the proxy at `at`, asking for `name`, offering `alpn`.
pub async fn connect(
    pki: &Pki,
    at: SocketAddr,
    name: &str,
    alpn: &[&[u8]],
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    tls_over(pki, TcpStream::connect(at).await?, name, alpn).await
}

/// TLS over `tcp`, asking for `name`, offering `alpn`.
pub async fn tls_over(
    pki: &Pki,
    tcp: TcpStream,
    name: &str,
    alpn: &[&[u8]],
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    tls_as(pki, tcp, (name, alpn), None).await
}

/// TLS over `tcp`, asking for `name` and offering `alpn`, presenting `identity` if asked for a
/// certificate and given one.
pub async fn tls_as(
    pki: &Pki,
    tcp: TcpStream,
    (name, alpn): (&str, &[&[u8]]),
    identity: Option<Identity>,
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(pki.roots());
    let mut config = match identity {
        Some((chain, key)) => builder.with_client_auth_cert(chain, key).unwrap(),
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let name = ServerName::try_from(name.to_owned()).unwrap();
    tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp).await
}

/// `req` sent over HTTPS to the proxy, asking for `name` in SNI, over HTTP/2 when `h2`; the
/// response's status and body.
pub async fn https(
    pki: &Pki,
    at: SocketAddr,
    name: &str,
    h2: bool,
    req: Request<Empty<Bytes>>,
) -> (StatusCode, String) {
    let alpn: &[&[u8]] = if h2 { &[b"h2"] } else { &[b"http/1.1"] };
    let io = TokioIo::new(connect(pki, at, name, alpn).await.unwrap());
    let response = if h2 {
        let exec = hyper_util::rt::TokioExecutor::new();
        let (mut send, conn) = hyper::client::conn::http2::handshake(exec, io).await.unwrap();
        tokio::spawn(conn);
        send.send_request(req).await.unwrap()
    } else {
        let (mut send, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(conn);
        send.send_request(req).await.unwrap()
    };
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A GET of `uri` naming `host`.
pub fn get(uri: &str, host: &str) -> Request<Empty<Bytes>> {
    Request::get(uri).header(header::HOST, host).body(Empty::new()).unwrap()
}

/// `raw` written to `at` and everything answered until the connection closes.
pub async fn raw(at: SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(at).await.unwrap();
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    String::from_utf8_lossy(&out).into_owned()
}
