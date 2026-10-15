// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains: `korpis.v1.Domain` (§3.2 of docs/07-networking.md).

use crate::{timestamp, wire_id};
use korpis_core::dns::domain::{Domain, Verification};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a verification method.
#[must_use]
pub fn verification_to_pb(v: Verification) -> pb::DomainVerification {
    match v {
        Verification::DnsTxt => pb::DomainVerification::DOMAIN_VERIFICATION_DNS_TXT,
        Verification::Delegated => pb::DomainVerification::DOMAIN_VERIFICATION_DELEGATED,
        Verification::Operator => pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR,
    }
}

/// The wire form of a domain. Its token is shown with the record that must hold it: it proves
/// nothing until it is published in the zone, which only the zone's holder can do.
#[must_use]
pub fn domain_to_pb(d: &Domain) -> pb::Domain {
    let txt = d.verification == Verification::DnsTxt;
    pb::Domain {
        id: wire_id(IdKind::Domain, d.id),
        organization: wire_id(IdKind::Organization, d.organization),
        name: d.name.as_str().to_owned(),
        verification: verification_to_pb(d.verification).into(),
        record: if txt { d.name.verification_record() } else { String::new() },
        token: d.token.clone(),
        parent: d.parent.map(|p| wire_id(IdKind::Domain, p)),
        verified_at: d.verified_at.map(timestamp).into(),
        created_at: timestamp(d.created_at).into(),
        ..Default::default()
    }
}
