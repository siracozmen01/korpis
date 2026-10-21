// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals: `korpis.v1.Portal` (§3.6 of docs/07-networking.md), with what its certificate says of
//! itself, so whoever reads a portal learns whether it can be served and why not.

use crate::certificate::{source_to_pb, state_to_pb};
use crate::{timestamp, wire_id};
use korpis_core::dns::certificate::Certificate;
use korpis_core::dns::portal::Portal;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a portal, served with `certificate`.
#[must_use]
pub fn portal_to_pb(p: &Portal, certificate: Option<&Certificate>) -> pb::Portal {
    let mut out = pb::Portal {
        id: wire_id(IdKind::Portal, p.id),
        organization: wire_id(IdKind::Organization, p.organization),
        name: p.name.as_str().to_owned(),
        certificate: wire_id(IdKind::Certificate, p.certificate),
        created_at: timestamp(p.created_at).into(),
        ..Default::default()
    };
    if let Some(c) = certificate {
        out.certificate_source = source_to_pb(c.source).into();
        out.certificate_state = state_to_pb(c.state).into();
        out.certificate_reason = c.reason.clone().unwrap_or_default();
    }
    out
}
