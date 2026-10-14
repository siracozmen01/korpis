// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the proxy does with a request it has authenticated: connect to where it may go, then
//! tunnel or forward.

use super::{Head, Rules, WAIT};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncWriteExt as _, copy, copy_bidirectional};
use tokio::net::TcpStream;

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
    copy_bidirectional(&mut client, &mut server).await.map(|_| ())
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
    let ((mut client_reads, mut client_writes), (mut server_reads, mut server_writes)) =
        (client.into_split(), server.into_split());
    let body = tokio::spawn(async move { copy(&mut client_reads, &mut server_writes).await });
    let answered = copy(&mut server_reads, &mut client_writes).await;
    body.abort();
    client_writes.shutdown().await?;
    answered.map(|_| ())
}
