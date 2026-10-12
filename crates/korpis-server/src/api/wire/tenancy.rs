// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations and projects.

use super::super::error::invalid;
use super::{timestamp, wire_id};
use connectrpc::ConnectError;
use korpis_core::id::IdKind;
use korpis_core::labels::Labels;
use korpis_core::tenancy::{Organization, OrganizationStatus, Project};
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
            OrganizationStatus::Suspended => pb::OrganizationStatus::ORGANIZATION_STATUS_SUSPENDED,
        }
        .into(),
        created: timestamp(o.created).into(),
        version: o.version,
        ..Default::default()
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
) -> Result<Labels, ConnectError> {
    Labels::new(pairs.into_iter().map(|(k, v)| (k.clone(), v.clone())))
        .map_err(|e| invalid(field, e.to_string()))
}
