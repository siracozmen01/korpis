// SPDX-License-Identifier: AGPL-3.0-or-later

//! The kernel's connection tracking, read and pruned over its netlink family: every IPv4 and IPv6
//! connection with the tuples it began and is answered with, and a connection forgotten by its
//! original tuple. An edge forgets the connections of an address it stops holding and of a port
//! whose target moved, so a UDP flow, which a client keeps alive with every datagram, is not
//! left translated to a node its workload left (§3.1 of docs/07-networking.md).

use netlink_sys::protocols::NETLINK_NETFILTER;
use netlink_sys::{Socket, SocketAddr};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The connection-tracking subsystem's messages, and the two used here.
const CTNETLINK: u16 = 1 << 8;
const GET: u16 = 1;
const DELETE: u16 = 2;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
/// An attribute's type, without the nested and byte-order flags.
const TYPE: u16 = 0x3fff;
const CTA_TUPLE_ORIG: u16 = 1;
const CTA_TUPLE_REPLY: u16 = 2;
const CTA_TUPLE_IP: u16 = 1;
const CTA_TUPLE_PROTO: u16 = 2;
const CTA_IP_V4_SRC: u16 = 1;
const CTA_IP_V4_DST: u16 = 2;
const CTA_IP_V6_SRC: u16 = 3;
const CTA_IP_V6_DST: u16 = 4;
const CTA_PROTO_NUM: u16 = 1;
const CTA_PROTO_SRC_PORT: u16 = 2;
const CTA_PROTO_DST_PORT: u16 = 3;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;

/// One direction of a connection as conntrack keys it; the ports are zero for a protocol
/// without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tuple {
    pub(crate) source: (IpAddr, u16),
    pub(crate) destination: (IpAddr, u16),
    pub(crate) protocol: u8,
}

/// One connection: as it began, as it is answered, which after a translation names where it
/// went, and its original tuple as the kernel gave it, which forgetting it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Connection {
    pub(crate) original: Tuple,
    pub(crate) reply: Tuple,
    raw: Vec<u8>,
}

/// Each attribute of `bytes`, with its type and its payload; and the attribute whole.
fn attributes(bytes: &[u8]) -> Vec<(u16, &[u8], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&[a, b, c, d]) = bytes.get(at..at + 4) {
        let len = usize::from(u16::from_ne_bytes([a, b]));
        let kind = u16::from_ne_bytes([c, d]) & TYPE;
        let (Some(payload), Some(whole)) = (bytes.get(at + 4..at + len), bytes.get(at..at + len))
        else {
            break;
        };
        if len < 4 {
            break;
        }
        out.push((kind, payload, whole));
        at += len.next_multiple_of(4);
    }
    out
}

fn find<'a>(attrs: &[(u16, &'a [u8], &'a [u8])], kind: u16) -> Option<&'a [u8]> {
    attrs.iter().find(|(k, _, _)| *k == kind).map(|(_, p, _)| *p)
}

/// The address of `ip`, a tuple's addresses, that is `v4` or `v6` of its family.
fn address(ip: &[(u16, &[u8], &[u8])], (v4, v6): (u16, u16)) -> Option<IpAddr> {
    if let Some(p) = find(ip, v4) {
        return <[u8; 4]>::try_from(p).ok().map(|o| IpAddr::V4(Ipv4Addr::from(o)));
    }
    <[u8; 16]>::try_from(find(ip, v6)?).ok().map(|o| IpAddr::V6(Ipv6Addr::from(o)))
}

fn port(payload: Option<&[u8]>) -> u16 {
    payload.and_then(|p| <[u8; 2]>::try_from(p).ok()).map_or(0, u16::from_be_bytes)
}

/// A tuple's attributes read.
fn tuple(payload: &[u8]) -> Option<Tuple> {
    let attrs = attributes(payload);
    let ip = attributes(find(&attrs, CTA_TUPLE_IP)?);
    let proto = attributes(find(&attrs, CTA_TUPLE_PROTO)?);
    let source = address(&ip, (CTA_IP_V4_SRC, CTA_IP_V6_SRC))?;
    let destination = address(&ip, (CTA_IP_V4_DST, CTA_IP_V6_DST))?;
    Some(Tuple {
        source: (source, port(find(&proto, CTA_PROTO_SRC_PORT))),
        destination: (destination, port(find(&proto, CTA_PROTO_DST_PORT))),
        protocol: *find(&proto, CTA_PROTO_NUM)?.first()?,
    })
}

/// A connection read from the attributes of one message.
fn connection(body: &[u8]) -> Option<Connection> {
    let attrs = attributes(body);
    let (_, original, raw) = attrs.iter().find(|(k, _, _)| *k == CTA_TUPLE_ORIG)?;
    Some(Connection {
        original: tuple(original)?,
        reply: tuple(find(&attrs, CTA_TUPLE_REPLY)?)?,
        raw: raw.to_vec(),
    })
}

/// A socket to the kernel's connection tracking in the calling thread's namespace.
pub(crate) struct Conntrack {
    socket: Socket,
    seq: u32,
}

impl Conntrack {
    /// Opens it.
    pub(crate) fn open() -> io::Result<Self> {
        let mut socket = Socket::new(NETLINK_NETFILTER)?;
        socket.bind_auto()?;
        socket.connect(&SocketAddr::new(0, 0))?;
        Ok(Self { socket, seq: 0 })
    }

    /// Sends a message of `kind` with `flags` and `body` for the address `family`, and gives each
    /// message answering it, as its type and what follows its header, to `each`, until the
    /// kernel is done or acks.
    fn exchange(
        &mut self,
        (kind, flags, family): (u16, u16, u8),
        body: &[u8],
        mut each: impl FnMut(u16, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.seq = self.seq.wrapping_add(1);
        let len = u32::try_from(20 + body.len()).map_err(io::Error::other)?;
        let mut message = Vec::with_capacity(20 + body.len());
        message.extend_from_slice(&len.to_ne_bytes());
        message.extend_from_slice(&(CTNETLINK | kind).to_ne_bytes());
        message.extend_from_slice(&flags.to_ne_bytes());
        message.extend_from_slice(&self.seq.to_ne_bytes());
        message.extend_from_slice(&0_u32.to_ne_bytes());
        message.extend_from_slice(&[family, 0, 0, 0]);
        message.extend_from_slice(body);
        self.socket.send(&message, 0)?;
        loop {
            let (datagram, _) = self.socket.recv_from_full()?;
            if self.read(&datagram, &mut each)? {
                return Ok(());
            }
        }
    }

    /// Reads one datagram's messages to `each`; whether the exchange is over.
    fn read(
        &self,
        datagram: &[u8],
        each: &mut impl FnMut(u16, &[u8]) -> io::Result<()>,
    ) -> io::Result<bool> {
        let mut at = 0;
        while let Some(head) = datagram.get(at..at + 16) {
            let word = |i: usize| head.get(i..i + 4).and_then(|b| b.try_into().ok());
            let (Some(len), Some(seq)) = (word(0).map(u32::from_ne_bytes), word(8)) else {
                break;
            };
            let len = usize::try_from(len).map_err(io::Error::other)?;
            let kind = head.get(4..6).and_then(|b| b.try_into().ok()).map(u16::from_ne_bytes);
            let body = datagram.get(at + 16..at + len.max(16)).unwrap_or_default();
            at += len.max(16).next_multiple_of(4);
            if u32::from_ne_bytes(seq) != self.seq {
                continue;
            }
            match kind {
                Some(NLMSG_DONE) => return Ok(true),
                Some(NLMSG_ERROR) => {
                    let code =
                        body.get(..4).and_then(|b| b.try_into().ok()).map(i32::from_ne_bytes);
                    return match code {
                        Some(0) => Ok(true),
                        Some(c) => Err(io::Error::from_raw_os_error(-c)),
                        None => Err(io::Error::other("a short netlink error")),
                    };
                }
                Some(k) => each(k, body.get(4..).unwrap_or_default())?,
                None => break,
            }
        }
        Ok(false)
    }

    /// Every IPv4 and IPv6 connection the kernel tracks.
    pub(crate) fn connections(&mut self) -> io::Result<Vec<Connection>> {
        let mut out = Vec::new();
        for family in [AF_INET, AF_INET6] {
            self.exchange((GET, NLM_F_REQUEST | NLM_F_DUMP, family), &[], |_, body| {
                out.extend(connection(body));
                Ok(())
            })?;
        }
        Ok(out)
    }

    /// Forgets `c`; one gone already is no error.
    pub(crate) fn forget(&mut self, c: &Connection) -> io::Result<()> {
        let body = c.raw.clone();
        let family = if c.original.source.0.is_ipv4() { AF_INET } else { AF_INET6 };
        let asked = (DELETE, NLM_F_REQUEST | NLM_F_ACK, family);
        match self.exchange(asked, &body, |_, _| Ok(())) {
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests;
