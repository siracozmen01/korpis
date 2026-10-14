// SPDX-License-Identifier: AGPL-3.0-or-later

//! How repositories and backups are stored, row by row, and read back into the domain.

use crate::stored::to_time;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::backup::{Backup, BackupStats, BackupStatus, KeyCustody, Repository, Retention};
use std::time::Duration;
use uuid::Uuid;

pub(super) type Time = chrono::DateTime<chrono::Utc>;

/// One stored repository.
pub(super) struct RepositoryRow {
    pub(super) id: Uuid,
    pub(super) organization_id: Uuid,
    pub(super) project_id: Option<Uuid>,
    pub(super) name: String,
    pub(super) provider_name: String,
    pub(super) prefix: String,
    pub(super) key_custody: String,
    pub(super) keep_last: i32,
    pub(super) keep_hourly: i32,
    pub(super) keep_daily: i32,
    pub(super) keep_weekly: i32,
    pub(super) keep_monthly: i32,
    pub(super) keep_yearly: i32,
    pub(super) keep_failed_s: i64,
    pub(super) min_retention_s: i64,
    pub(super) created_at: Time,
    pub(super) deleted_at: Option<Time>,
}

fn count(v: i32) -> StoreResult<u32> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("a retention count {v}")))
}

fn seconds(v: i64) -> StoreResult<Duration> {
    u64::try_from(v)
        .map(Duration::from_secs)
        .map_err(|_| StoreError::Corrupt(format!("a duration {v}")))
}

impl RepositoryRow {
    pub(super) fn into_domain(self) -> StoreResult<Repository> {
        let custody = KeyCustody::parse(&self.key_custody)
            .ok_or_else(|| StoreError::Corrupt(format!("key custody {}", self.key_custody)))?;
        let retention = Retention {
            keep_last: count(self.keep_last)?,
            keep_hourly: count(self.keep_hourly)?,
            keep_daily: count(self.keep_daily)?,
            keep_weekly: count(self.keep_weekly)?,
            keep_monthly: count(self.keep_monthly)?,
            keep_yearly: count(self.keep_yearly)?,
            keep_failed: seconds(self.keep_failed_s)?,
            min_retention: seconds(self.min_retention_s)?,
        };
        Ok(Repository {
            id: self.id,
            organization: self.organization_id,
            project: self.project_id,
            name: self.name,
            provider: self.provider_name,
            prefix: self.prefix,
            custody,
            retention,
            created: to_time(self.created_at),
            deleted_at: self.deleted_at.map(to_time),
        })
    }
}

/// One stored backup.
pub(super) struct BackupRow {
    pub(super) id: Uuid,
    pub(super) repository_id: Uuid,
    pub(super) workload_id: Uuid,
    pub(super) volume_id: Uuid,
    pub(super) plan_id: Uuid,
    pub(super) parent_id: Option<Uuid>,
    pub(super) status: String,
    pub(super) consistent: bool,
    pub(super) files: i64,
    pub(super) directories: i64,
    pub(super) bytes: i64,
    pub(super) chunks_new: i64,
    pub(super) bytes_new: i64,
    pub(super) stored_bytes: i64,
    pub(super) files_unchanged: i64,
    pub(super) skipped: Vec<String>,
    pub(super) error: Option<String>,
    pub(super) created_at: Time,
    pub(super) finished_at: Option<Time>,
    pub(super) deleted_at: Option<Time>,
}

pub(super) fn unsigned(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

pub(super) fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl BackupRow {
    pub(super) fn into_domain(self) -> StoreResult<Backup> {
        let status = BackupStatus::parse(&self.status)
            .ok_or_else(|| StoreError::Corrupt(format!("backup status {}", self.status)))?;
        Ok(Backup {
            id: self.id,
            repository: self.repository_id,
            workload: self.workload_id,
            volume: self.volume_id,
            plan: self.plan_id,
            parent: self.parent_id,
            status,
            consistent: self.consistent,
            stats: BackupStats {
                files: unsigned(self.files),
                directories: unsigned(self.directories),
                bytes: unsigned(self.bytes),
                chunks_new: unsigned(self.chunks_new),
                bytes_new: unsigned(self.bytes_new),
                stored_bytes: unsigned(self.stored_bytes),
                files_unchanged: unsigned(self.files_unchanged),
                skipped: self.skipped,
            },
            error: self.error,
            created: to_time(self.created_at),
            finished: self.finished_at.map(to_time),
            deleted_at: self.deleted_at.map(to_time),
        })
    }
}
