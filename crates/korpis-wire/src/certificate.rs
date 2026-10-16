// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates: `korpis.v1.Certificate` (§3.2 of docs/07-networking.md), never with a key.

use crate::{timestamp, wire_id};
use korpis_core::dns::certificate::{Certificate, Source, State};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The wire form of where a certificate comes from.
#[must_use]
pub fn source_to_pb(s: Source) -> pb::CertificateSource {
    match s {
        Source::Acme => pb::CertificateSource::CERTIFICATE_SOURCE_ACME,
        Source::Provided => pb::CertificateSource::CERTIFICATE_SOURCE_PROVIDED,
    }
}

/// The wire form of where a certificate is in its life.
#[must_use]
pub fn state_to_pb(s: State) -> pb::CertificateState {
    match s {
        State::Pending => pb::CertificateState::CERTIFICATE_STATE_PENDING,
        State::Issued => pb::CertificateState::CERTIFICATE_STATE_ISSUED,
        State::Failed => pb::CertificateState::CERTIFICATE_STATE_FAILED,
    }
}

/// The wire form of a certificate.
#[must_use]
pub fn certificate_to_pb(c: &Certificate) -> pb::Certificate {
    pb::Certificate {
        id: wire_id(IdKind::Certificate, c.id),
        organization: wire_id(IdKind::Organization, c.organization),
        names: c.names.clone(),
        source: source_to_pb(c.source).into(),
        chain: c.chain.clone(),
        not_before: c.not_before.map(timestamp).into(),
        not_after: c.not_after.map(timestamp).into(),
        state: state_to_pb(c.state).into(),
        reason: c.reason.clone().unwrap_or_default(),
        renew_after: c.renew_after.map(timestamp).into(),
        revision: c.revision,
        created_at: timestamp(c.created_at).into(),
        ..Default::default()
    }
}
