// SPDX-License-Identifier: AGPL-3.0-or-later

//! The little of a DNS message the resolver reads (RFC 1035 §4.1): enough to refuse a query in
//! its own terms and to match an answer to its query. Everything else is forwarded as it came.

/// A message's header length.
const HEADER: usize = 12;

/// A query's identifier, if it is long enough to be a message at all.
#[must_use]
pub fn id(message: &[u8]) -> Option<[u8; 2]> {
    Some([*message.first()?, *message.get(1)?])
}

/// The end of the question section of a query with one question, if it is well formed.
fn question_end(query: &[u8]) -> Option<usize> {
    let mut at = HEADER;
    loop {
        let len = usize::from(*query.get(at)?);
        if len == 0 {
            break;
        }
        // A query's name is never compressed; a pointer here is not one this reads.
        if len & 0xc0 != 0 {
            return None;
        }
        at = at.checked_add(1 + len)?;
    }
    let end = at.checked_add(1 + 4)?;
    (end <= query.len()).then_some(end)
}

/// The answer that refuses `query` (RCODE 5, `REFUSED`), which a resolver library treats as a
/// failure to retry elsewhere rather than as an answer: its identifier, opcode, and question,
/// and nothing else. `None` for what is not a query.
#[must_use]
pub fn refused(query: &[u8]) -> Option<Vec<u8>> {
    let header = query.get(..HEADER)?;
    let flags = header.get(2)?;
    if flags & 0x80 != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([*header.get(4)?, *header.get(5)?]);
    let end = if questions == 1 { question_end(query) } else { None };
    let mut answer = Vec::with_capacity(end.unwrap_or(HEADER));
    answer.extend_from_slice(header.get(..2)?);
    // QR set, the opcode and RD kept; RA set, RCODE 5.
    answer.push(0x80 | (flags & 0x79));
    answer.push(0x80 | 5);
    answer.extend_from_slice(&[0, u8::from(end.is_some()), 0, 0, 0, 0, 0, 0]);
    if let Some(end) = end {
        answer.extend_from_slice(query.get(HEADER..end)?);
    }
    Some(answer)
}

/// The resolvers a `resolv.conf` names, on `port`, in order; a scoped IPv6 address is left
/// out, since the scope names an interface of the host's that the resolver does not bind to.
#[must_use]
pub fn upstreams(conf: &str, port: u16) -> Vec<std::net::SocketAddr> {
    conf.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.trim().parse::<std::net::IpAddr>().ok())
        .map(|ip| std::net::SocketAddr::new(ip, port))
        .collect()
}
