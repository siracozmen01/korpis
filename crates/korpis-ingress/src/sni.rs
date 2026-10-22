// SPDX-License-Identifier: AGPL-3.0-or-later

//! The name a TLS client asks for (RFC 8446 §4.1.2, RFC 6066 §3), and the protocols it offers
//! (RFC 7301), read before anything is decided: whether the connection is a directory validating
//! a name by TLS-ALPN-01 (RFC 8737), passed through to a workload unread, which needs the bytes
//! exactly as they came, or terminated here. So the client's first handshake message is read
//! whole into memory, at most [`MAX_HELLO`] bytes of it, and kept to be replayed.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// The most of a `ClientHello` read: a TLS record's payload is at most 16 KiB, and a hello may
/// span records, but a client sending more than this before it is answered is refused.
pub const MAX_HELLO: usize = 32 << 10;

/// A record of the handshake protocol.
const HANDSHAKE: u8 = 22;
/// The handshake message that opens a connection.
const CLIENT_HELLO: u8 = 1;
/// The extension naming the server.
const SERVER_NAME: u16 = 0;
/// A name of type `host_name` in it.
const HOST_NAME: u8 = 0;
/// The extension listing the protocols a client offers.
const ALPN: u16 = 16;
/// The one protocol a TLS-ALPN-01 validation offers.
pub const ACME_TLS: &[u8] = b"acme-tls/1";

/// What a client sent first: every byte read, and the name it asked for, if it asked for one.
#[derive(Debug)]
pub struct Hello {
    /// The bytes read, to be replayed.
    pub read: Vec<u8>,
    /// The `host_name` of its `server_name` extension, lowercase.
    pub name: Option<String>,
    /// The protocols its ALPN extension offers, in its order.
    pub protocols: Vec<Vec<u8>>,
}

impl Hello {
    /// Whether it offers `acme-tls/1` and nothing else, as a TLS-ALPN-01 validation does.
    #[must_use]
    pub fn acme_only(&self) -> bool {
        !self.protocols.is_empty() && self.protocols.iter().all(|p| p == ACME_TLS)
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// Reads the client's `ClientHello` from `io`.
pub async fn read_hello(io: &mut (impl AsyncRead + Unpin)) -> io::Result<Hello> {
    let mut read = Vec::new();
    let mut message = Vec::new();
    // The handshake message's length is in its first four bytes; read records until it is whole.
    while message.len() < 4 || message.len() < 4 + message_len(&message) {
        let mut header = [0u8; 5];
        io.read_exact(&mut header).await?;
        let [kind, _, _, high, low] = header;
        if kind != HANDSHAKE {
            return Err(invalid("not a TLS handshake"));
        }
        let len = usize::from(u16::from_be_bytes([high, low]));
        if read.len() + 5 + len > MAX_HELLO {
            return Err(invalid("a ClientHello above the limit"));
        }
        let mut payload = vec![0u8; len];
        io.read_exact(&mut payload).await?;
        read.extend_from_slice(&header);
        read.extend_from_slice(&payload);
        message.extend_from_slice(&payload);
    }
    if message.first() != Some(&CLIENT_HELLO) {
        return Err(invalid("the first handshake message is not a ClientHello"));
    }
    let body = message.get(4..4 + message_len(&message)).ok_or_else(|| invalid("short"))?;
    let (name, protocols) = extensions(body)?;
    Ok(Hello { read, name, protocols })
}

/// The length a handshake message's header gives, once there are four bytes of it.
fn message_len(m: &[u8]) -> usize {
    match m {
        [_, a, b, c, ..] => usize::from(*a) << 16 | usize::from(*b) << 8 | usize::from(*c),
        _ => 0,
    }
}

/// A cursor over a message, every read checked.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    /// A vector with a length prefix of `width` bytes.
    fn vector(&mut self, width: u8) -> Option<&'a [u8]> {
        let len = if width == 1 { usize::from(self.u8()?) } else { usize::from(self.u16()?) };
        self.take(len)
    }
}

/// A part of a `ClientHello` that should be there and is not.
fn malformed<T>(part: Option<T>) -> io::Result<T> {
    part.ok_or_else(|| invalid("a malformed ClientHello"))
}

/// The `host_name` a `ClientHello`'s body asks for, if it asks for one, and the protocols it
/// offers.
fn extensions(body: &[u8]) -> io::Result<(Option<String>, Vec<Vec<u8>>)> {
    let mut c = Cursor(body);
    malformed(c.take(2 + 32))?; // legacy_version, random
    malformed(c.vector(1))?; // legacy_session_id
    malformed(c.vector(2))?; // cipher_suites
    malformed(c.vector(1))?; // legacy_compression_methods
    let (mut name, mut protocols) = (None, Vec::new());
    if c.0.is_empty() {
        return Ok((name, protocols));
    }
    let mut extensions = Cursor(malformed(c.vector(2))?);
    while !extensions.0.is_empty() {
        let kind = malformed(extensions.u16())?;
        let data = malformed(extensions.vector(2))?;
        match kind {
            SERVER_NAME => name = Some(malformed(host_name(data))?),
            ALPN => protocols = malformed(offered(data))?,
            _ => {}
        }
    }
    Ok((name, protocols))
}

/// The protocols an ALPN extension lists (RFC 7301 §3.1).
fn offered(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut list = Cursor(Cursor(data).vector(2)?);
    let mut out = Vec::new();
    while !list.0.is_empty() {
        out.push(list.vector(1)?.to_vec());
    }
    Some(out)
}

fn host_name(data: &[u8]) -> Option<String> {
    let mut list = Cursor(Cursor(data).vector(2)?);
    while !list.0.is_empty() {
        let kind = list.u8()?;
        let name = list.vector(2)?;
        if kind == HOST_NAME {
            let name = std::str::from_utf8(name).ok()?;
            return Some(name.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests;
