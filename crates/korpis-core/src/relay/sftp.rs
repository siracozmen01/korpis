// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a relay serves SFTP (§3.1 of docs/02-architecture.md, §3 of docs/18-operations.md): the
//! address clients reach it at, `host:port`, which a relay behind a balancer or a port forward is
//! told with `--sftp-advertise` and otherwise takes from its own URL's host and the port it
//! listens on; and the line a client's `known_hosts` takes for it.

use super::{RelayUrl, RelayUrlError};
use std::fmt;

/// The port SSH is reached at when none is named.
pub const SSH_PORT: u16 = 22;

/// Where clients reach an SFTP server: a host name or an IP address, and a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpAddress {
    host: String,
    port: u16,
}

impl SftpAddress {
    /// The address `host:port`, or `[v6]:port`, names; the port is required.
    ///
    /// # Errors
    /// It is not a host and a port.
    pub fn parse(s: &str) -> Result<Self, RelayUrlError> {
        let refuse = || RelayUrlError(s.to_owned());
        let (_, port) = s.rsplit_once(':').ok_or_else(refuse)?;
        if port.is_empty() || s.ends_with(']') {
            return Err(refuse());
        }
        let url = RelayUrl::parse(&format!("https://{s}")).map_err(|_| refuse())?;
        Ok(Self { host: url.host().to_owned(), port: url.port() })
    }

    /// The address at `url`'s host and `port`: where a relay is reached unless told otherwise.
    #[must_use]
    pub fn at(url: &RelayUrl, port: u16) -> Self {
        Self { host: url.host().to_owned(), port }
    }

    /// The host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// How `known_hosts` names it: the host alone on port 22, `[host]:port` otherwise.
    #[must_use]
    pub fn known_hosts_name(&self) -> String {
        if self.port == SSH_PORT {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for SftpAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn an_address_is_a_host_and_a_port_and_is_named_as_known_hosts_names_it() {
        let a = SftpAddress::parse("relay.example:2022").unwrap();
        assert_eq!((a.host(), a.port()), ("relay.example", 2022));
        assert_eq!(
            (a.to_string(), a.known_hosts_name()),
            ("relay.example:2022".into(), "[relay.example]:2022".into())
        );
        let v6 = SftpAddress::parse("[2001:db8::1]:22").unwrap();
        assert_eq!(
            (v6.to_string(), v6.known_hosts_name()),
            ("[2001:db8::1]:22".into(), "2001:db8::1".into())
        );
        for bad in ["relay.example", "relay.example:", ":22", "relay.example:0", "a b:22", "[::1]"]
        {
            assert!(SftpAddress::parse(bad).is_err(), "{bad}");
        }
        let url = RelayUrl::parse("https://fra-relay.example").unwrap();
        assert_eq!(SftpAddress::at(&url, 2022).to_string(), "fra-relay.example:2022");
    }
}
