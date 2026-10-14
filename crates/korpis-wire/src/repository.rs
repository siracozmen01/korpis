// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories, their retention policies, and backups (§5 of docs/06-storage.md).

use crate::{Result, duration, duration_from, id, invalid, timestamp, wire_id};
use korpis_core::backup::{
    Backup, BackupStats, BackupStatus, KeyCustody, ReportedBackup, Repository, Retention,
};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
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

/// The wire form of a backup.
#[must_use]
pub fn backup_to_pb(b: &Backup) -> pb::Backup {
    let status = match b.status {
        BackupStatus::Pending => pb::BackupStatus::BACKUP_STATUS_PENDING,
        BackupStatus::Succeeded => pb::BackupStatus::BACKUP_STATUS_SUCCEEDED,
        BackupStatus::Failed => pb::BackupStatus::BACKUP_STATUS_FAILED,
    };
    let s = &b.stats;
    pb::Backup {
        id: wire_id(IdKind::Backup, b.id),
        repository: wire_id(IdKind::Repository, b.repository),
        workload: wire_id(IdKind::Workload, b.workload),
        volume: wire_id(IdKind::Volume, b.volume),
        plan: wire_id(IdKind::Plan, b.plan),
        parent: b.parent.map(|p| wire_id(IdKind::Backup, p)),
        status: status.into(),
        consistent: b.consistent,
        files: s.files,
        directories: s.directories,
        bytes: s.bytes,
        chunks_new: s.chunks_new,
        bytes_new: s.bytes_new,
        stored_bytes: s.stored_bytes,
        files_unchanged: s.files_unchanged,
        skipped: s.skipped.clone(),
        error: b.error.clone().unwrap_or_default(),
        created: timestamp(b.created).into(),
        finished: b.finished.map(timestamp).into(),
        deleted_at: b.deleted_at.map(timestamp).into(),
        ..Default::default()
    }
}

/// What a node reports of a backup job that ended, from the field `field`. The backup arrives as
/// the bare identifier the plan's step carried.
pub fn reported_backup_from_pb(field: &str, e: &agent::BackupEvent) -> Result<ReportedBackup> {
    let backup = uuid::Uuid::parse_str(&e.backup)
        .map_err(|_| invalid(&format!("{field}.backup"), "the backup's identifier"))?;
    let result =
        e.result.as_option().ok_or_else(|| invalid(&format!("{field}.result"), "required"))?;
    let record = result.backup.as_option();
    let stats = record.and_then(|b| b.stats.as_option()).map(|s| BackupStats {
        files: s.files,
        directories: s.directories,
        bytes: s.bytes,
        chunks_new: s.chunks_new,
        bytes_new: s.bytes_new,
        stored_bytes: s.stored_bytes,
        files_unchanged: s.files_unchanged,
        skipped: s.skipped.clone(),
    });
    Ok(ReportedBackup {
        workload: id(IdKind::Workload, &format!("{field}.workload"), &e.workload)?,
        backup,
        // Whole only with its record: a job that says it succeeded without one did not.
        ok: result.ok && record.is_some(),
        error: (!result.error.is_empty()).then(|| result.error.clone()),
        consistent: record.is_some_and(|b| b.consistent),
        stats: stats.unwrap_or_default(),
    })
}
