// SPDX-License-Identifier: AGPL-3.0-or-later

//! An ingress endpoint's names in DNS (§3.5 of docs/07-networking.md): each name of an endpoint
//! the ingress serves resolves to the addresses its node advertises, `A` and `AAAA`, published
//! through a DNS provider of the endpoint's organization that serves the name's zone, for an owner
//! of its own. What was asked, what the provider last made it, and through which provider, are
//! kept, so a move re-publishes it through the same provider and a name no longer routed is
//! withdrawn from it.

use super::{DnsRecord, RecordType};
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a resolver keeps an endpoint's address: a minute, so a workload that moves is found
/// at its new node a minute after its records change.
pub const ADDRESS_TTL: u32 = 60;

/// How long a name no provider serves waits before the providers are asked again, one installed
/// meanwhile among them.
pub const UNMANAGED_RECHECK: Duration = Duration::from_secs(3600);

/// Where a name's records stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// Not yet asked of any provider.
    Pending,
    /// A provider made them so.
    Published,
    /// No DNS provider of the organization serves the name's zone: its holder publishes them.
    Unmanaged,
    /// The provider serving the zone refused, or could not be reached.
    Failed,
}

impl RecordState {
    /// Its name, as stored and shown.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Published => "published",
            Self::Unmanaged => "unmanaged",
            Self::Failed => "failed",
        }
    }

    /// The state `s` names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Pending, Self::Published, Self::Unmanaged, Self::Failed]
            .into_iter()
            .find(|r| r.as_str() == s)
    }
}

/// One name of an ingress endpoint, and its records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRecord {
    /// The endpoint.
    pub endpoint: Uuid,
    /// The name.
    pub name: String,
    /// The endpoint's workload.
    pub workload: Uuid,
    /// Its organization, as whom providers are called.
    pub organization: Uuid,
    /// The owner the records are applied for.
    pub owner: String,
    /// The addresses it should resolve to; none once nothing routes it, and it is withdrawn.
    pub wanted: Option<Vec<IpAddr>>,
    /// What the provider last made it.
    pub applied: Option<Vec<IpAddr>>,
    /// The provider that publishes it, or was last asked to.
    pub provider: Option<String>,
    /// Where it stands.
    pub state: RecordState,
    /// Why, when it is not published.
    pub reason: Option<String>,
    /// Attempts in a row that failed.
    pub attempts: u32,
    /// When it is looked at again, if not at its next change.
    pub next_attempt: Option<SystemTime>,
}

/// The records that make `name` resolve to `addresses`: one `A` set and one `AAAA` set, each
/// present only if an address of its family is.
#[must_use]
pub fn address_records(name: &str, addresses: &[IpAddr]) -> Vec<DnsRecord> {
    let family = |kind: RecordType, v4: bool| {
        let values: Vec<String> =
            addresses.iter().filter(|a| a.is_ipv4() == v4).map(ToString::to_string).collect();
        (!values.is_empty()).then(|| DnsRecord {
            name: name.to_owned(),
            kind,
            ttl: ADDRESS_TTL,
            values,
        })
    };
    family(RecordType::A, true).into_iter().chain(family(RecordType::Aaaa, false)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_resolves_to_each_family_its_node_advertises() {
        let both: Vec<IpAddr> = ["203.0.113.7", "2001:db8::7", "203.0.113.8"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect();
        let made = address_records("shop.example.com", &both);
        assert_eq!(made.len(), 2);
        assert_eq!(
            (made[0].kind, made[0].values.clone()),
            (RecordType::A, vec!["203.0.113.7".to_owned(), "203.0.113.8".to_owned()])
        );
        assert_eq!(
            (made[1].kind, made[1].values.clone()),
            (RecordType::Aaaa, vec!["2001:db8::7".to_owned()])
        );
        assert!(super::super::check_records("ep_x/0", &made).is_ok());
        assert!(address_records("shop.example.com", &[]).is_empty(), "nothing to resolve to");
        assert_eq!(RecordState::parse("unmanaged"), Some(RecordState::Unmanaged));
    }
}
