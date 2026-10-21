// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals (§3.6 of docs/07-networking.md, §3.6 of docs/01-model.md): a name an organization
//! holds, under which the control plane serves its API and panel for that organization. A
//! portal's name is held as an endpoint's names are, by the organization whose verified domain is
//! the most specific one covering it; it is no endpoint's route and no other portal's; and it is
//! never one of the installation's own hosts, which the control plane serves already.

use super::domain::DomainName;
use std::time::SystemTime;
use uuid::Uuid;

/// A name the control plane answers under for an organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Portal {
    /// Its identifier.
    pub id: Uuid,
    /// The organization whose theme and identity providers it wears, beneath which a session
    /// begun at it acts.
    pub organization: Uuid,
    /// The name, which is the relying party's identifier for every passkey registered at it.
    pub name: DomainName,
    /// The certificate it is served with: its own `acme` one, or a `provided` one of its
    /// organization covering its name.
    pub certificate: Uuid,
    /// When it was made.
    pub created_at: SystemTime,
}

impl Portal {
    /// Its origin, for an installation reached on `port`: `https://<name>`, with the port only
    /// where it is not HTTPS's own.
    #[must_use]
    pub fn origin(&self, port: Option<u16>) -> String {
        match port {
            Some(p) if p != 443 => format!("https://{}:{p}", self.name.as_str()),
            _ => format!("https://{}", self.name.as_str()),
        }
    }
}

/// Whether `name` is one of `hosts`, the installation's own: the host of its public URL and of
/// each `--web-origin`, compared without case, as hosts are.
#[must_use]
pub fn installations_own(name: &DomainName, hosts: &[String]) -> bool {
    hosts.iter().any(|h| h.eq_ignore_ascii_case(name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal(name: &str) -> Portal {
        Portal {
            id: Uuid::nil(),
            organization: Uuid::nil(),
            name: DomainName::parse(name).unwrap(),
            certificate: Uuid::nil(),
            created_at: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn its_origin_names_the_port_only_where_it_is_not_https_s_own() {
        let p = portal("panel.reseller.example");
        assert_eq!(p.origin(None), "https://panel.reseller.example");
        assert_eq!(p.origin(Some(443)), "https://panel.reseller.example");
        assert_eq!(p.origin(Some(7443)), "https://panel.reseller.example:7443");
    }

    #[test]
    fn the_installation_s_own_hosts_are_never_a_portal() {
        let hosts = ["Korpis.Example.com".to_owned(), "panel.example.com".to_owned()];
        let own = |n: &str| installations_own(&DomainName::parse(n).unwrap(), &hosts);
        assert!(own("korpis.example.com"));
        assert!(own("panel.example.com"));
        assert!(!own("panel.reseller.example"));
    }
}
