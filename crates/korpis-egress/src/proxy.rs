// SPDX-License-Identifier: AGPL-3.0-or-later

//! A proxy for clients that take no resolver: on loopback, for this process alone (it asks a
//! credential made when it starts), reaching only what [`Reach`] permits, and the registries the
//! operator named as spoken to over plain HTTP. HTTPS is tunnelled with `CONNECT`; a plain HTTP
//! request is forwarded, one to a connection, so a client reusing a connection to the proxy cannot
//! send its second request to the first one's server.
//!
//! It runs on a thread of its own with its own runtime, so a client is built with it the same way
//! whatever runtime builds the client, and it stops when it is dropped.

mod forward;

use super::Reach;
use base64::Engine as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};

/// The most a request's head may be, in bytes.
const MAX_HEAD: usize = 16 << 10;

/// How long a client has to send its request's head, and a server to accept a connection.
const WAIT: Duration = Duration::from_secs(30);

/// A running proxy.
#[derive(Debug)]
pub struct Proxy {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

/// What every connection is checked against.
#[derive(Debug)]
struct Rules {
    reach: Reach,
    /// Registries spoken to over plain HTTP, as `host:port`, which the operator named.
    named: Vec<String>,
    /// The `Proxy-Authorization` header a request must carry.
    credential: String,
}

impl Proxy {
    /// Starts a proxy reaching what `reach` permits, and the registries in `named`.
    ///
    /// # Errors
    /// No port could be bound, or no thread started.
    pub fn start(reach: Reach, named: Vec<String>) -> std::io::Result<Self> {
        let mut secret = [0u8; 24];
        getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
        let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("korpis:{password}"));
        let rules = Arc::new(Rules { reach, named, credential: format!("Basic {basic}") });
        let listener = std::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        listener.set_nonblocking(true)?;
        let at = listener.local_addr()?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        std::thread::Builder::new().name("korpis-egress".into()).spawn(move || {
            runtime.block_on(async move {
                match TcpListener::from_std(listener) {
                    Ok(l) => tokio::select! {
                        () = serve(l, rules) => {}
                        _ = stopped => {}
                    },
                    Err(e) => tracing::error!(error = %e, "the egress proxy could not listen"),
                }
            });
        })?;
        Ok(Self { url: format!("http://korpis:{password}@{at}"), stop: Some(stop) })
    }

    /// The proxy's URL, with its credential, for a client's HTTPS and HTTP proxy alike.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn serve(listener: TcpListener, rules: Arc<Rules>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let rules = Arc::clone(&rules);
        tokio::spawn(async move {
            if let Err(e) = connection(stream, &rules).await {
                tracing::debug!(error = %e, "an egress connection ended");
            }
        });
    }
}

/// A request's head: its request line's parts, its header lines, and what was read past it.
struct Head {
    method: String,
    target: String,
    version: String,
    headers: Vec<String>,
    rest: Vec<u8>,
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<Head> {
    let mut buf = Vec::new();
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::other("a request head over 16 KiB"));
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("the client closed before its request"));
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
    };
    let rest = buf.split_off(end + 4);
    let text = String::from_utf8(buf).map_err(|_| std::io::Error::other("a head not UTF-8"))?;
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());
    let line = lines.next().unwrap_or_default();
    let mut parts = line.split(' ');
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) if parts.next().is_none() => (m, t, v),
        _ => return Err(std::io::Error::other("a malformed request line")),
    };
    let headers = lines.map(str::to_owned).collect();
    Ok(Head {
        method: method.into(),
        target: target.into(),
        version: version.into(),
        headers,
        rest,
    })
}

/// The value of header `name` in `head`, if it is there.
fn header<'h>(head: &'h Head, name: &str) -> Option<&'h str> {
    head.headers.iter().find_map(|h| {
        let (n, v) = h.split_once(':')?;
        n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn connection(mut client: TcpStream, rules: &Rules) -> std::io::Result<()> {
    let head = tokio::time::timeout(WAIT, read_head(&mut client))
        .await
        .map_err(|_| std::io::Error::other("no request head in time"))??;
    if header(&head, "proxy-authorization") != Some(rules.credential.as_str()) {
        return forward::refuse(&mut client, "407 Proxy Authentication Required").await;
    }
    if head.method.eq_ignore_ascii_case("CONNECT") {
        forward::tunnel(client, &head, rules).await
    } else {
        forward::request(client, head, rules).await
    }
}
