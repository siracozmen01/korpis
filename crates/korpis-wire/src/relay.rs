// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays that run as their own process: `korpis.v1.Relay` (§3.1 of docs/02-architecture.md).

use crate::{timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::relay::Relay;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a relay.
#[must_use]
pub fn relay_to_pb(r: &Relay) -> pb::Relay {
    pb::Relay {
        id: wire_id(IdKind::Relay, r.id),
        name: r.name.as_str().to_owned(),
        url: r.url.as_str().to_owned(),
        joined_at: timestamp(r.joined_at).into(),
        certificate_expires_at: r.certificate_expires_at.map(timestamp).into(),
        removed_at: r.removed_at.map(timestamp).into(),
        ..Default::default()
    }
}
