// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays (§3.1 of docs/02-architecture.md): the stateless processes that join a client's session
//! to a node's outbound data connection. A relay that runs as its own process is enrolled like a
//! node, once, and is then known by its identity key and the URL clients and nodes reach it at.
//! The control plane's own relay role is not one of these: it needs no enrollment, since it is
//! the control plane.

use crate::tenancy::Name;
use std::time::SystemTime;
use uuid::Uuid;

/// A relay running as its own process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relay {
    /// Immutable.
    pub id: Uuid,
    /// For humans; unique in the installation.
    pub name: Name,
    /// Where clients and nodes reach it, as `https://host[:port]`.
    pub url: RelayUrl,
    /// When it joined.
    pub joined_at: SystemTime,
    /// When the certificate the control plane last issued it expires.
    pub certificate_expires_at: Option<SystemTime>,
    /// When it was removed; a removed relay is told nothing more and named to no node.
    pub removed_at: Option<SystemTime>,
}

/// Where a relay is reached: an `https` URL with a host, an optional port, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUrl {
    url: String,
    host: String,
}

/// Why a URL cannot name a relay.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a relay URL is https://host[:port] with nothing after it: {0}")]
pub struct RelayUrlError(pub String);

impl RelayUrl {
    /// `s`, if it is `https://host[:port]` (a trailing slash is dropped).
    pub fn parse(s: &str) -> Result<Self, RelayUrlError> {
        let refuse = || RelayUrlError(s.to_owned());
        let rest = s.strip_prefix("https://").ok_or_else(refuse)?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
            return Err(refuse());
        }
        let host = if let Some(v6) = rest.strip_prefix('[') {
            // An IPv6 literal, then an optional port.
            let (host, after) = v6.split_once(']').ok_or_else(refuse)?;
            valid_port(after).ok_or_else(refuse)?;
            host.parse::<std::net::Ipv6Addr>().map_err(|_| refuse())?;
            host.to_owned()
        } else {
            let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
            if !port.is_empty() {
                valid_port(&format!(":{port}")).ok_or_else(refuse)?;
            }
            if host.is_empty() {
                return Err(refuse());
            }
            host.to_ascii_lowercase()
        };
        Ok(Self { url: format!("https://{}", rest.to_ascii_lowercase()), host })
    }

    /// The URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// The host: the name its certificate is issued for.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// `Some` when `after` is empty or `:` and a port number.
fn valid_port(after: &str) -> Option<()> {
    if after.is_empty() {
        return Some(());
    }
    after.strip_prefix(':')?.parse::<u16>().ok().filter(|p| *p > 0).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_is_named_by_an_https_origin() {
        let u = RelayUrl::parse("https://Relay-1.Example.net:8443/").unwrap();
        assert_eq!(
            (u.as_str(), u.host()),
            ("https://relay-1.example.net:8443", "relay-1.example.net")
        );
        let v6 = RelayUrl::parse("https://[2001:db8::1]:443").unwrap();
        assert_eq!(v6.host(), "2001:db8::1");
        for bad in [
            "http://relay",
            "https://",
            "https://relay/path",
            "https://relay:0",
            "https://relay:99999",
            "https://user@relay",
            "https://[not-v6]",
            "https://:443",
        ] {
            assert!(RelayUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
