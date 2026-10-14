// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories and their retention policies (§5.3 of docs/06-storage.md).

use crate::{Result, duration, duration_from, invalid, timestamp, wire_id};
use korpis_core::backup::{KeyCustody, Repository, Retention};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a custody.
#[must_use]
pub fn custody_to_pb(c: KeyCustody) -> pb::KeyCustody {
    match c {
        KeyCustody::Operator => pb::KeyCustody::KEY_CUSTODY_OPERATOR,
        KeyCustody::Tenant => pb::KeyCustody::KEY_CUSTODY_TENANT,
    }
}

/// A custody from the field `field`; unset is the operator's, the default of §5.5.
pub fn custody_from_pb(field: &str, c: buffa::EnumValue<pb::KeyCustody>) -> Result<KeyCustody> {
    match c.as_known() {
        Some(pb::KeyCustody::KEY_CUSTODY_UNSPECIFIED | pb::KeyCustody::KEY_CUSTODY_OPERATOR) => {
            Ok(KeyCustody::Operator)
        }
        Some(pb::KeyCustody::KEY_CUSTODY_TENANT) => Ok(KeyCustody::Tenant),
        None => Err(invalid(field, "operator or tenant")),
    }
}

/// The wire form of a retention policy.
#[must_use]
pub fn retention_to_pb(r: &Retention) -> pb::RetentionPolicy {
    pb::RetentionPolicy {
        keep_last: r.keep_last,
        keep_hourly: r.keep_hourly,
        keep_daily: r.keep_daily,
        keep_weekly: r.keep_weekly,
        keep_monthly: r.keep_monthly,
        keep_yearly: r.keep_yearly,
        keep_failed: duration(r.keep_failed).into(),
        min_retention: duration(r.min_retention).into(),
        ..Default::default()
    }
}

/// A retention policy from the field `field`.
pub fn retention_from_pb(field: &str, r: &pb::RetentionPolicy) -> Result<Retention> {
    let optional = |name: &str, d: Option<&buffa_types::google::protobuf::Duration>| {
        d.map(|d| duration_from(&format!("{field}.{name}"), d))
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(Retention {
        keep_last: r.keep_last,
        keep_hourly: r.keep_hourly,
        keep_daily: r.keep_daily,
        keep_weekly: r.keep_weekly,
        keep_monthly: r.keep_monthly,
        keep_yearly: r.keep_yearly,
        keep_failed: optional("keep_failed", r.keep_failed.as_option())?,
        min_retention: optional("min_retention", r.min_retention.as_option())?,
    })
}

/// The wire form of a repository.
#[must_use]
pub fn repository_to_pb(r: &Repository) -> pb::Repository {
    pb::Repository {
        id: wire_id(IdKind::Repository, r.id),
        organization: wire_id(IdKind::Organization, r.organization),
        project: r.project.map(|p| wire_id(IdKind::Project, p)),
        name: r.name.clone(),
        provider: r.provider.clone(),
        prefix: r.prefix.clone(),
        key_custody: custody_to_pb(r.custody).into(),
        retention: retention_to_pb(&r.retention).into(),
        created: timestamp(r.created).into(),
        ..Default::default()
    }
}
