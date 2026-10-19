// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations and projects.

use crate::invalid;
use crate::{timestamp, wire_id};

use korpis_core::id::IdKind;
use korpis_core::labels::Labels;
use korpis_core::tenancy::{OnSuspend, Organization, OrganizationStatus, Project, Suspension};
use korpis_proto::korpis::v1 as pb;

/// The wire form of an organization.
#[must_use]
pub fn organization_to_pb(o: &Organization) -> pb::Organization {
    pb::Organization {
        id: wire_id(IdKind::Organization, o.id),
        name: o.name.as_str().to_owned(),
        parent: o.parent.map(|p| wire_id(IdKind::Organization, p)),
        status: match o.status {
            OrganizationStatus::Active => pb::OrganizationStatus::ORGANIZATION_STATUS_ACTIVE,
            OrganizationStatus::Suspended(_) => {
                pb::OrganizationStatus::ORGANIZATION_STATUS_SUSPENDED
            }
        }
        .into(),
        suspension: match &o.status {
            OrganizationStatus::Suspended(s) => suspension_to_pb(s).into(),
            OrganizationStatus::Active => None.into(),
        },
        created: timestamp(o.created).into(),
        version: o.version,
        ..Default::default()
    }
}

/// The wire form of a suspension.
#[must_use]
pub fn suspension_to_pb(s: &Suspension) -> pb::Suspension {
    pb::Suspension {
        by: crate::principal::to_pb(&s.by).into(),
        at: timestamp(s.at).into(),
        reason: s.reason.clone(),
        on_suspend: match s.on_suspend {
            OnSuspend::KeepRunning => pb::OnSuspend::ON_SUSPEND_KEEP_RUNNING,
            OnSuspend::Stop => pb::OnSuspend::ON_SUSPEND_STOP,
        }
        .into(),
        ..Default::default()
    }
}

/// What a request's `on_suspend` asks for: keeping the workloads running unless it says stop.
#[must_use]
pub fn on_suspend(v: pb::OnSuspend) -> OnSuspend {
    match v {
        pb::OnSuspend::ON_SUSPEND_STOP => OnSuspend::Stop,
        pb::OnSuspend::ON_SUSPEND_KEEP_RUNNING | pb::OnSuspend::ON_SUSPEND_UNSPECIFIED => {
            OnSuspend::KeepRunning
        }
    }
}

/// The wire form of a project.
#[must_use]
pub fn project_to_pb(p: &Project) -> pb::Project {
    pb::Project {
        id: wire_id(IdKind::Project, p.id),
        organization: wire_id(IdKind::Organization, p.organization),
        name: p.name.as_str().to_owned(),
        labels: p.labels.as_map().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        created: timestamp(p.created).into(),
        version: p.version,
        ..Default::default()
    }
}

/// Labels from the request field `field`.
pub fn labels<'a>(
    field: &str,
    pairs: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> Result<Labels, crate::WireError> {
    Labels::new(pairs.into_iter().map(|(k, v)| (k.clone(), v.clone())))
        .map_err(|e| invalid(field, e.to_string()))
}
