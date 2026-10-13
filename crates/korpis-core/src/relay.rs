// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays (§3.1 of docs/02-architecture.md): the stateless processes that join a client's session
//! to a node's outbound data connection. A relay that runs as its own process is enrolled like a
//! node, once, and is then known by its identity key and the URL clients and nodes reach it at.
//! The control plane's own relay role is not one of these: it needs no enrollment, since it is
//! the control plane.

use crate::event::Severity;
use crate::tenancy::Name;
use std::time::{Duration, SystemTime};
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

/// How close a relay's certificate is to expiring, as an event says it (§7 of
/// docs/18-operations.md): a warning within [`WARN_WITHIN`], critical within [`CRITICAL_WITHIN`],
/// and nothing before. Each step is reported once; a renewal starts the count again.
#[must_use]
pub fn certificate_alert(expires_at: SystemTime, now: SystemTime) -> Option<Severity> {
    let left = expires_at.duration_since(now).unwrap_or_default();
    if left <= CRITICAL_WITHIN {
        Some(Severity::Critical)
    } else if left <= WARN_WITHIN {
        Some(Severity::Warning)
    } else {
        None
    }
}

/// A relay's certificate expires within this: a warning.
pub const WARN_WITHIN: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// Within this: critical.
pub const CRITICAL_WITHIN: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// A relay's certificate came a step closer to expiring: an event, keyed to the relay, in the
/// root organization whose relays they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateAlert {
    /// The relay.
    pub relay: Uuid,
    /// Its name.
    pub name: String,
    /// The root organization.
    pub organization: Uuid,
    /// When its certificate expires.
    pub expires_at: SystemTime,
    /// How close that is.
    pub severity: Severity,
    /// When it was decided, on the control plane's clock.
    pub at: SystemTime,
}

impl CertificateAlert {
    /// The event's kind.
    pub const KIND: &'static str = "relay.certificate_expiring";

    /// The event's key: one ordered history per relay.
    #[must_use]
    pub fn key(&self) -> String {
        format!("relay:{}", self.relay)
    }
}

/// Where a relay is reached: an `https` URL with a host, an optional port, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayUrl {
    url: String,
    host: String,
    port: u16,
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
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            // An IPv6 literal, then an optional port.
            let (host, after) = v6.split_once(']').ok_or_else(refuse)?;
            let port = valid_port(after).ok_or_else(refuse)?;
            host.parse::<std::net::Ipv6Addr>().map_err(|_| refuse())?;
            (host.to_owned(), port)
        } else {
            let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
            let port = if rest.contains(':') {
                valid_port(&format!(":{port}")).ok_or_else(refuse)?
            } else {
                HTTPS_PORT
            };
            if host.is_empty() {
                return Err(refuse());
            }
            (host.to_ascii_lowercase(), port)
        };
        Ok(Self { url: format!("https://{}", rest.to_ascii_lowercase()), host, port })
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

    /// The port: the one it names, or 443.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// The port an `https` URL names when it names none.
const HTTPS_PORT: u16 = 443;

/// The port, when `after` is empty (443) or `:` and a port number.
fn valid_port(after: &str) -> Option<u16> {
    if after.is_empty() {
        return Some(HTTPS_PORT);
    }
    after.strip_prefix(':')?.parse::<u16>().ok().filter(|p| *p > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_is_alerted_on_twice_as_it_nears_expiry() {
        let now = SystemTime::now();
        let in_days = |d: u64| now + Duration::from_secs(d * 24 * 60 * 60);
        assert_eq!(certificate_alert(in_days(30), now), None);
        assert_eq!(certificate_alert(in_days(14), now), Some(Severity::Warning));
        assert_eq!(certificate_alert(in_days(3), now), Some(Severity::Critical));
        assert_eq!(certificate_alert(now - Duration::from_secs(1), now), Some(Severity::Critical));
    }

    #[test]
    fn a_relay_is_named_by_an_https_origin() {
        let u = RelayUrl::parse("https://Relay-1.Example.net:8443/").unwrap();
        assert_eq!(
            (u.as_str(), u.host()),
            ("https://relay-1.example.net:8443", "relay-1.example.net")
        );
        assert_eq!(u.port(), 8443);
        let v6 = RelayUrl::parse("https://[2001:db8::1]:444").unwrap();
        assert_eq!((v6.host(), v6.port()), ("2001:db8::1", 444));
        assert_eq!(RelayUrl::parse("https://relay").unwrap().port(), 443);
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
