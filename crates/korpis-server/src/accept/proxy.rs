// SPDX-License-Identifier: AGPL-3.0-or-later

//! The PROXY protocol, versions 1 and 2, as `HAProxy` specifies it: the header a load balancer
//! writes before a connection's first byte, naming the client it accepted the connection from.
//! Read exactly, and never past the header, so what follows is the client's TLS handshake.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// The version 2 signature.
const V2: [u8; 12] = [0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a];
/// The longest version 1 header, its line ending included.
const V1_MAX: usize = 107;

/// What a header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// The connection was accepted from this client.
    Client(SocketAddr),
    /// The connection is the proxy's own (a health check), or its client is not an IP address.
    Local,
}

/// Why a header could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// The connection failed or ended inside the header.
    #[error("reading the PROXY header: {0}")]
    Io(#[from] std::io::Error),
    /// What arrived is not a PROXY header.
    #[error("not a PROXY header: {0}")]
    Malformed(&'static str),
}

/// Reads one header from `r`, and nothing after it.
pub async fn read_header<R: AsyncRead + Unpin>(r: &mut R) -> Result<Header, ProxyError> {
    let mut start = [0u8; 12];
    r.read_exact(&mut start).await?;
    if start == V2 {
        return read_v2(r).await;
    }
    if !start.starts_with(b"PROXY ") {
        return Err(ProxyError::Malformed("neither version 1 nor version 2"));
    }
    let mut line = start.to_vec();
    while !line.ends_with(b"\r\n") {
        if line.len() >= V1_MAX {
            return Err(ProxyError::Malformed("a version 1 line longer than 107 bytes"));
        }
        line.push(r.read_u8().await?);
    }
    parse_v1(&line)
}

async fn read_v2<R: AsyncRead + Unpin>(r: &mut R) -> Result<Header, ProxyError> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head).await?;
    let [version_command, family, high, low] = head;
    if version_command >> 4 != 2 {
        return Err(ProxyError::Malformed("version 2 signature with another version"));
    }
    let mut rest = vec![0u8; usize::from(u16::from_be_bytes([high, low]))];
    r.read_exact(&mut rest).await?;
    match version_command & 0x0f {
        0 => Ok(Header::Local),
        1 => addresses_v2(family, &rest),
        _ => Err(ProxyError::Malformed("an unknown version 2 command")),
    }
}

/// The source of a version 2 PROXY command's address block.
fn addresses_v2(family: u8, block: &[u8]) -> Result<Header, ProxyError> {
    let short = || ProxyError::Malformed("an address block shorter than its family");
    match family >> 4 {
        1 => {
            let b: &[u8; 12] = block.get(..12).and_then(|b| b.try_into().ok()).ok_or_else(short)?;
            let ip = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
            Ok(Header::Client(SocketAddr::new(IpAddr::V4(ip), u16::from_be_bytes([b[8], b[9]]))))
        }
        2 => {
            let b: &[u8; 36] = block.get(..36).and_then(|b| b.try_into().ok()).ok_or_else(short)?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&b[..16]);
            let port = u16::from_be_bytes([b[32], b[33]]);
            Ok(Header::Client(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port)))
        }
        // UNSPEC, or a Unix socket: no IP address to name.
        _ => Ok(Header::Local),
    }
}

fn parse_v1(line: &[u8]) -> Result<Header, ProxyError> {
    let text = std::str::from_utf8(line).map_err(|_| ProxyError::Malformed("not ASCII"))?;
    let mut words = text.trim_end_matches("\r\n").split(' ').skip(1);
    let bad = || ProxyError::Malformed("a version 1 line that does not parse");
    match words.next() {
        Some("UNKNOWN") => Ok(Header::Local),
        Some("TCP4" | "TCP6") => {
            let ip: IpAddr = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
            let _destination = words.next().ok_or_else(bad)?;
            let port: u16 = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
            Ok(Header::Client(SocketAddr::new(ip, port)))
        }
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(bytes: &[u8]) -> (Result<Header, ProxyError>, Vec<u8>) {
        let mut r = bytes;
        let header = read_header(&mut r).await;
        (header, r.to_vec())
    }

    #[tokio::test]
    async fn version_1_names_the_client_and_stops_at_its_line() {
        let (h, rest) = read(b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\r\n\x16\x03").await;
        assert_eq!(h.unwrap(), Header::Client("203.0.113.7:51234".parse().unwrap()));
        assert_eq!(rest, b"\x16\x03", "the TLS handshake is left unread");
        let (h, _) = read(b"PROXY TCP6 2001:db8::7 2001:db8::1 51234 443\r\n").await;
        assert_eq!(h.unwrap(), Header::Client("[2001:db8::7]:51234".parse().unwrap()));
        assert_eq!(read(b"PROXY UNKNOWN\r\n").await.0.unwrap(), Header::Local);
        assert!(read(b"GET / HTTP/1.1\r\n").await.0.is_err());
        let long = format!("PROXY TCP4 {}\r\n", "1".repeat(120));
        assert!(read(long.as_bytes()).await.0.is_err());
    }

    #[tokio::test]
    async fn version_2_names_the_client_and_skips_what_follows_the_addresses() {
        let mut v4 = V2.to_vec();
        v4.extend_from_slice(&[0x21, 0x11, 0, 16]);
        v4.extend_from_slice(&[203, 0, 113, 7, 10, 0, 0, 1, 0xc8, 0x22, 0x01, 0xbb]);
        v4.extend_from_slice(&[0x04, 0, 1, 0]);
        v4.push(0x16);
        let (h, rest) = read(&v4).await;
        assert_eq!(h.unwrap(), Header::Client("203.0.113.7:51234".parse().unwrap()));
        assert_eq!(rest, [0x16], "the TLVs are consumed, the handshake is not");
        let mut local = V2.to_vec();
        local.extend_from_slice(&[0x20, 0x00, 0, 0]);
        assert_eq!(read(&local).await.0.unwrap(), Header::Local);
        let mut short = V2.to_vec();
        short.extend_from_slice(&[0x21, 0x21, 0, 12]);
        short.extend_from_slice(&[0; 12]);
        assert!(read(&short).await.0.is_err(), "an IPv6 block needs 36 bytes");
    }
}
