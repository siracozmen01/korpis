// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a node's proxy proves an `acme` endpoint's name that no DNS provider proves (§3.2 of
//! docs/07-networking.md): over HTTP-01 on port 80, or by TLS-ALPN-01 on port 443. The issuer
//! cannot tell which port of a node a directory reaches, so it tries the way that last proved the
//! certificate first, HTTP-01 for one never issued, and the other after each failed attempt.

/// A way a node's proxy proves a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeProof {
    /// The key authorization served on port 80.
    Http01,
    /// A certificate made for the challenge, served on port 443 to `acme-tls/1` (RFC 8737).
    TlsAlpn01,
}

impl NodeProof {
    /// The other way.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Http01 => Self::TlsAlpn01,
            Self::TlsAlpn01 => Self::Http01,
        }
    }

    /// The way attempt `attempts + 1` is made, after `attempts` failed in a row, for a
    /// certificate `last` proved, if anything did: `last`, or HTTP-01, first, and the two in turn
    /// after that.
    #[must_use]
    pub const fn for_attempt(last: Option<Self>, attempts: u32) -> Self {
        let first = match last {
            Some(way) => way,
            None => Self::Http01,
        };
        if attempts.is_multiple_of(2) { first } else { first.other() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_way_that_last_proved_it_is_tried_first_and_the_two_alternate_after_a_failure() {
        use NodeProof::{Http01 as H, TlsAlpn01 as T};
        let ways = |last| (0..4).map(|a| NodeProof::for_attempt(last, a)).collect::<Vec<_>>();
        assert_eq!(ways(None), [H, T, H, T], "never issued: HTTP-01 first");
        assert_eq!(ways(Some(T)), [T, H, T, H], "a node whose port 80 is closed keeps TLS-ALPN-01");
        assert_eq!(ways(Some(H)), [H, T, H, T]);
    }
}
