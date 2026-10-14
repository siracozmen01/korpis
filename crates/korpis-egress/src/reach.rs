// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which addresses a request made on a tenant's behalf may reach: the public internet, and the
//! networks the operator names. The policy, and the resolver a `reqwest` client uses to apply it:
//! every address a name resolves to is checked by the resolver the connection then uses, so a name
//! cannot pass the check and connect elsewhere.

use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

/// The networks a request may reach beyond the public internet: the operator's, by name.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    allowed: Arc<Vec<IpNet>>,
}

impl Reach {
    /// The public internet and `allowed`.
    #[must_use]
    pub fn with(allowed: Vec<IpNet>) -> Self {
        Self { allowed: Arc::new(allowed) }
    }

    /// Whether a request may connect to `ip`.
    #[must_use]
    pub fn permits(&self, ip: IpAddr) -> bool {
        public(ip) || self.allowed.iter().any(|n| n.contains(&ip))
    }

    /// Whether a URL may be fetched, for what its host says before any name is resolved: an
    /// address written in the URL is checked here, a name where it is resolved.
    ///
    /// # Errors
    /// The URL names an address that may not be reached, or no host.
    pub fn url(&self, url: &reqwest::Url) -> Result<(), String> {
        match url.host() {
            Some(::url::Host::Ipv4(ip)) => self.address(IpAddr::V4(ip)),
            Some(::url::Host::Ipv6(ip)) => self.address(IpAddr::V6(ip)),
            Some(::url::Host::Domain(_)) => Ok(()),
            None => Err("a URL names no host".into()),
        }
    }

    /// Whether `ip` may be reached, as an error saying why not.
    ///
    /// # Errors
    /// It may not.
    pub fn address(&self, ip: IpAddr) -> Result<(), String> {
        if self.permits(ip) {
            Ok(())
        } else {
            Err(format!("{ip} is not on the public internet nor a network the operator allows"))
        }
    }
}

impl Reach {
    /// The addresses `host` resolves to that may be reached, with `port`; an error when there are
    /// none, naming the host and not what it resolved to.
    ///
    /// # Errors
    /// It does not resolve, or resolves to no address that may be reached.
    pub async fn addresses(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        let found = tokio::net::lookup_host((host, port)).await?;
        let kept: Vec<SocketAddr> = found.filter(|a| self.permits(a.ip())).collect();
        if kept.is_empty() {
            return Err(std::io::Error::other(format!(
                "{host} resolves to no address on the public internet nor on a network the \
                 operator allows"
            )));
        }
        Ok(kept)
    }
}

impl Resolve for Reach {
    fn resolve(&self, name: Name) -> Resolving {
        let reach = self.clone();
        Box::pin(async move {
            let kept = reach.addresses(name.as_str(), 0).await?;
            let addrs: Addrs = Box::new(kept.into_iter());
            Ok(addrs)
        })
    }
}

/// A redirect policy for a client reaching only what `reach` permits: at most ten redirects, each
/// to `http` or `https` at an address that may be reached. A name is checked when it is resolved.
#[must_use]
pub fn redirects(reach: &Reach) -> reqwest::redirect::Policy {
    let reach = reach.clone();
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("more than ten redirects")
        } else if !matches!(attempt.url().scheme(), "http" | "https") {
            attempt.error("a redirect to neither http nor https")
        } else if let Err(e) = reach.url(attempt.url()) {
            attempt.error(e)
        } else {
            attempt.follow()
        }
    })
}

/// Whether `ip` is a unicast address on the public internet.
#[must_use]
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

/// IPv4 ranges that are not the public internet (RFC 6890 and its successors).
const NOT_PUBLIC_V4: &[(u32, u8)] = &[
    (0x0000_0000, 8),  // "this network"
    (0x0a00_0000, 8),  // private
    (0x6440_0000, 10), // carrier-grade NAT
    (0x7f00_0000, 8),  // loopback
    (0xa9fe_0000, 16), // link-local, where cloud metadata services listen
    (0xac10_0000, 12), // private
    (0xc000_0000, 24), // protocol assignments
    (0xc000_0200, 24), // documentation
    (0xc058_6300, 24), // 6to4 relay anycast, deprecated
    (0xc0a8_0000, 16), // private
    (0xc612_0000, 15), // benchmarking
    (0xc633_6400, 24), // documentation
    (0xcb00_7100, 24), // documentation
    (0xe000_0000, 4),  // multicast
    (0xf000_0000, 4),  // reserved, and the broadcast address
];

fn public_v4(ip: Ipv4Addr) -> bool {
    let bits = u32::from(ip);
    !NOT_PUBLIC_V4
        .iter()
        .any(|&(net, len)| bits >> (32 - u32::from(len)) == net >> (32 - u32::from(len)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // An address that carries an IPv4 one is as public as that one: mapped, NAT64, and 6to4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return public_v4(Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7])));
    }
    if s[0] == 0x2002 {
        return public_v4(Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])));
    }
    // Global unicast is 2000::/3; within it, the IETF's special-purpose block (2001::/23, which
    // holds Teredo, benchmarking, and ORCHID) and documentation (2001:db8::/32, 3fff::/20) are
    // not reached.
    let global = s[0] & 0xe000 == 0x2000;
    let special =
        (s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0xdb8)) || (s[0] == 0x3fff && s[1] < 0x1000);
    global && !special
}

#[cfg(test)]
mod tests {
    use super::{Reach, public};
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn only_the_public_internet_is_reached_unless_the_operator_allows_more() {
        for p in ["1.1.1.1", "8.8.8.8", "151.101.1.1", "2606:4700::1111", "2a00:1450::1"] {
            assert!(public(ip(p)), "{p}");
        }
        let refused = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd00:ec2::254",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2001:db8::1",
            "2001::1",
        ];
        for r in refused {
            assert!(!public(ip(r)), "{r}");
        }
        assert!(public(ip("172.32.0.1")) && public(ip("100.128.0.1")), "the ranges' edges");
        let mirror = Reach::with(vec!["10.8.0.0/16".parse().unwrap_or_else(|_| unreachable!())]);
        assert!(mirror.permits(ip("10.8.3.4")) && !mirror.permits(ip("10.9.0.1")));
        assert!(!mirror.permits(ip("127.0.0.1")));
    }
}
