// SPDX-License-Identifier: AGPL-3.0-or-later

//! The event log's wire forms (§5 of docs/15-observability.md): filters, `korpis.v1.EventFilter`;
//! subscriptions, `korpis.v1.Subscription`; and event identifiers, which are positions.

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{IdKind, Result, id, invalid, timestamp, wire_id};
use korpis_core::event::{EventFilter, Severity, Subscription, position_id, position_of};
use korpis_proto::korpis::v1 as pb;

enum_map!(severity_to_pb, severity_from_pb, Severity, pb::Severity, "a severity", {
    Severity::Info => pb::Severity::SEVERITY_INFO,
    Severity::Warning => pb::Severity::SEVERITY_WARNING,
    Severity::Critical => pb::Severity::SEVERITY_CRITICAL,
});

/// The wire form of an event filter.
#[must_use]
pub fn filter_to_pb(f: &EventFilter) -> pb::EventFilter {
    pb::EventFilter {
        kinds: f.kinds.clone(),
        min_severity: severity_to_pb(f.min_severity).into(),
        projects: f.projects.iter().map(|p| wire_id(IdKind::Project, *p)).collect(),
        workloads: selector_to_pb(&f.workloads).into(),
        ..Default::default()
    }
}

/// A filter from the field `field`, as a subscriber writes it.
pub fn filter_from_pb(field: &str, f: &pb::EventFilter) -> Result<EventFilter> {
    let min_severity = match f.min_severity.as_known() {
        None | Some(pb::Severity::SEVERITY_UNSPECIFIED) => Severity::Info,
        Some(_) => severity_from_pb(&format!("{field}.min_severity"), f.min_severity)?,
    };
    let projects = f
        .projects
        .iter()
        .enumerate()
        .map(|(i, p)| id(IdKind::Project, &format!("{field}.projects[{i}]"), p))
        .collect::<Result<_>>()?;
    let workloads = selector_from_pb(&format!("{field}.workloads"), f.workloads.as_option())?;
    Ok(EventFilter { kinds: f.kinds.clone(), min_severity, projects, workloads })
}

/// An event's identifier on the wire.
#[must_use]
pub fn event_id(position: u64) -> String {
    wire_id(IdKind::Event, position_id(position))
}

/// The position an event identifier from the field `field` names.
pub fn position_from(field: &str, s: &str) -> Result<u64> {
    position_of(id(IdKind::Event, field, s)?).ok_or_else(|| invalid(field, "not an event"))
}

/// The wire form of a subscription.
#[must_use]
pub fn subscription_to_pb(s: &Subscription) -> pb::Subscription {
    pb::Subscription {
        id: wire_id(IdKind::Subscription, s.id),
        subject: crate::principal::to_pb(&s.subject).into(),
        filter: filter_to_pb(&s.filter).into(),
        acked: if s.acked == 0 { String::new() } else { event_id(s.acked) },
        created: timestamp(s.created).into(),
        extension: s.extension.map(|e| wire_id(IdKind::Extension, e)),
        ..Default::default()
    }
}
