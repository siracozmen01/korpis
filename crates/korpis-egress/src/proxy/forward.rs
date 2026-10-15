// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the proxy does with a request it has authenticated: connect to where it may go, then
//! tunnel or forward.

use super::{Head, Rules, WAIT};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// How long a connection may carry nothing either way before the proxy closes it. The clients
/// that use the proxy time out a stalled read sooner; this ends what they leave behind.
const IDLE: Duration = Duration::from_secs(600);

/// When either direction of a connection last moved, in milliseconds since it began.
struct Activity {
    began: Instant,
    last: AtomicU64,
}

impl Activity {
    fn new() -> Arc<Self> {
        Arc::new(Self { began: Instant::now(), last: AtomicU64::new(0) })
    }

    fn touch(&self) {
        let now = u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last.store(now, Ordering::Relaxed);
    }

    fn idle(&self) -> Duration {
        let last = Duration::from_millis(self.last.load(Ordering::Relaxed));
        self.began.elapsed().saturating_sub(last)
    }
}

/// Copies `from` into `to` until `from` ends, or the connection has been idle both ways for
/// [`IDLE`]; then shuts `to` down.
async fn pump(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    activity: Arc<Activity>,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let wait = IDLE.saturating_sub(activity.idle()).max(Duration::from_secs(1));
        let n = match tokio::time::timeout(wait, from.read(&mut buf)).await {
            Ok(read) => read?,
            Err(_) if activity.idle() >= IDLE => break,
            Err(_) => continue,
        };
        if n == 0 {
            break;
        }
        to.write_all(buf.get(..n).unwrap_or_default()).await?;
        activity.touch();
    }
    to.shutdown().await
}

/// Answers `status` and closes.
pub(super) async fn refuse(client: &mut TcpStream, status: &str) -> std::io::Result<()> {
    let challenge = if status.starts_with("407") {
        "proxy-authenticate: Basic realm=\"korpis\"\r\n"
    } else {
        ""
    };
    let answer =
        format!("HTTP/1.1 {status}\r\n{challenge}content-length: 0\r\nconnection: close\r\n\r\n");
    client.write_all(answer.as_bytes()).await?;
    client.shutdown().await
}

/// The host and port an authority (`host:port`, an IPv6 host in brackets) names; `default` when
/// it names no port.
fn authority(text: &str, default: u16) -> Option<(url::Host, u16)> {
    let parsed = url::Url::parse(&format!("http://{text}/")).ok()?;
    let port = parsed.port().unwrap_or(default);
    Some((parsed.host()?.to_owned(), port))
}

/// Connects to `host`:`port` if it may be reached: a registry the operator named at whatever it
/// resolves to, anything else only at an address the policy permits.
async fn reach(rules: &Rules, host: &url::Host, port: u16) -> std::io::Result<TcpStream> {
    let named =
        rules.named.iter().any(|n| authority(n, 80).is_some_and(|(h, p)| h == *host && p == port));
    let addresses: Vec<SocketAddr> = match host {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(*ip), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(*ip), port)],
        url::Host::Domain(name) if named => {
            tokio::net::lookup_host((name.as_str(), port)).await?.collect()
        }
        url::Host::Domain(name) => rules.reach.addresses(name, port).await?,
    };
    if !named {
        for a in &addresses {
            rules.reach.address(a.ip()).map_err(std::io::Error::other)?;
        }
    }
    let mut last = std::io::Error::other(format!("{host} resolves to nothing"));
    for a in addresses {
        match tokio::time::timeout(WAIT, TcpStream::connect(a)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = e,
            Err(_) => last = std::io::Error::other(format!("{a} did not answer in time")),
        }
    }
    Err(last)
}

/// `CONNECT host:port`: a tunnel to it, if it may be reached.
pub(super) async fn tunnel(
    mut client: TcpStream,
    head: &Head,
    rules: &Rules,
) -> std::io::Result<()> {
    let Some((host, port)) = authority(&head.target, 443) else {
        return refuse(&mut client, "400 Bad Request").await;
    };
    let mut server = match reach(rules, &host, port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!(target = %head.target, error = %e, "an egress connection refused");
            return refuse(&mut client, "403 Forbidden").await;
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    server.write_all(&head.rest).await?;
    let ((client_reads, client_writes), (server_reads, server_writes)) =
        (client.into_split(), server.into_split());
    let activity = Activity::new();
    let up = tokio::spawn(pump(client_reads, server_writes, Arc::clone(&activity)));
    let down = pump(server_reads, client_writes, activity).await;
    // The server's side ended: the client has its answer, and nothing more goes up.
    up.abort();
    down
}

/// A plain HTTP request in absolute form: forwarded to its server in origin form, with the proxy's
/// own headers taken out and `connection: close` put in, and the client's connection closed once
/// the server's answer ends, so the connection carries this one request and no other.
pub(super) async fn request(
    mut client: TcpStream,
    head: Head,
    rules: &Rules,
) -> std::io::Result<()> {
    let target = url::Url::parse(&head.target).ok().filter(|u| u.scheme() == "http");
    let Some((target, host)) = target.and_then(|u| Some((u.clone(), u.host()?.to_owned()))) else {
        return refuse(&mut client, "400 Bad Request").await;
    };
    let port = target.port().unwrap_or(80);
    let mut server = match reach(rules, &host, port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!(target = %head.target, error = %e, "an egress request refused");
            return refuse(&mut client, "403 Forbidden").await;
        }
    };
    let path = match target.query() {
        Some(q) => format!("{}?{q}", target.path()),
        None => target.path().to_owned(),
    };
    let mut out = format!("{} {path} {}\r\n", head.method, head.version);
    for h in &head.headers {
        let name = h.split_once(':').map_or("", |(n, _)| n.trim()).to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "proxy-authorization" | "proxy-connection" | "connection" | "keep-alive"
        ) {
            out.push_str(h);
            out.push_str("\r\n");
        }
    }
    out.push_str("connection: close\r\n\r\n");
    server.write_all(out.as_bytes()).await?;
    server.write_all(&head.rest).await?;
    let ((client_reads, client_writes), (server_reads, server_writes)) =
        (client.into_split(), server.into_split());
    let activity = Activity::new();
    let body = tokio::spawn(pump(client_reads, server_writes, Arc::clone(&activity)));
    let answered = pump(server_reads, client_writes, activity).await;
    body.abort();
    answered
}
