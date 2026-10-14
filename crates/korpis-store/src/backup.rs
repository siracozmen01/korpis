// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories, their sealed keys, and backups (§5 of docs/06-storage.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{BackupRead, BackupWrite, SealedKey, StoreError, StoreResult};
use korpis_core::backup::{Backup, BackupStats, BackupStatus, KeyCustody, Repository, Retention};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

type Time = chrono::DateTime<chrono::Utc>;

/// One stored repository.
struct RepositoryRow {
    id: Uuid,
    organization_id: Uuid,
    project_id: Option<Uuid>,
    name: String,
    provider_name: String,
    prefix: String,
    key_custody: String,
    keep_last: i32,
    keep_hourly: i32,
    keep_daily: i32,
    keep_weekly: i32,
    keep_monthly: i32,
    keep_yearly: i32,
    keep_failed_s: i64,
    min_retention_s: i64,
    created_at: Time,
    deleted_at: Option<Time>,
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
    fn into_domain(self) -> StoreResult<Repository> {
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
struct BackupRow {
    id: Uuid,
    repository_id: Uuid,
    workload_id: Uuid,
    volume_id: Uuid,
    plan_id: Uuid,
    parent_id: Option<Uuid>,
    status: String,
    consistent: bool,
    files: i64,
    directories: i64,
    bytes: i64,
    chunks_new: i64,
    bytes_new: i64,
    stored_bytes: i64,
    files_unchanged: i64,
    skipped: Vec<String>,
    error: Option<String>,
    created_at: Time,
    finished_at: Option<Time>,
    deleted_at: Option<Time>,
}

fn unsigned(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl BackupRow {
    fn into_domain(self) -> StoreResult<Backup> {
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

impl BackupRead for PgStore {
    async fn repository(&self, id: Uuid) -> StoreResult<Option<Repository>> {
        let row = sqlx::query_as!(
            RepositoryRow,
            "SELECT id, organization_id, project_id, name, provider_name, prefix, key_custody,
                    keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly,
                    keep_failed_s, min_retention_s, created_at, deleted_at
               FROM repository WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(RepositoryRow::into_domain).transpose()
    }

    async fn repositories(
        &self,
        organization: Uuid,
        project: Option<Uuid>,
        page: Page,
    ) -> StoreResult<Vec<Repository>> {
        let rows = sqlx::query_as!(
            RepositoryRow,
            "SELECT id, organization_id, project_id, name, provider_name, prefix, key_custody,
                    keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly,
                    keep_failed_s, min_retention_s, created_at, deleted_at
               FROM repository
              WHERE organization_id = $1 AND deleted_at IS NULL
                AND ($2::uuid IS NULL OR project_id IS NULL OR project_id = $2)
                AND ($3::uuid IS NULL OR id > $3)
              ORDER BY id LIMIT $4",
            organization,
            project,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RepositoryRow::into_domain).collect()
    }

    async fn backup(&self, id: Uuid) -> StoreResult<Option<Backup>> {
        let row = sqlx::query_file_as!(
            BackupRow,
            "queries/backup.sql",
            id,
            None::<Uuid>,
            true,
            None::<Uuid>,
            1_i64
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(BackupRow::into_domain).transpose()
    }

    async fn backups(&self, workload: Uuid, deleted: bool, page: Page) -> StoreResult<Vec<Backup>> {
        let rows = sqlx::query_file_as!(
            BackupRow,
            "queries/backup.sql",
            None::<Uuid>,
            workload,
            deleted,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(BackupRow::into_domain).collect()
    }
}

impl BackupWrite for PgTx {
    async fn insert_repository(&mut self, r: &Repository, kek: &SealedKey) -> StoreResult<()> {
        let t = &r.retention;
        let n = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let s = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        sqlx::query!(
            "INSERT INTO repository (id, organization_id, project_id, name, provider_name, prefix,
                                     key_custody, kek_sealed, kek_sealed_by, keep_last,
                                     keep_hourly, keep_daily, keep_weekly, keep_monthly,
                                     keep_yearly, keep_failed_s, min_retention_s, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18)",
            r.id,
            r.organization,
            r.project,
            r.name,
            r.provider,
            r.prefix,
            r.custody.as_str(),
            kek.sealed,
            kek.sealed_by,
            n(t.keep_last),
            n(t.keep_hourly),
            n(t.keep_daily),
            n(t.keep_weekly),
            n(t.keep_monthly),
            n(t.keep_yearly),
            s(t.keep_failed),
            s(t.min_retention),
            from_time(r.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn repository_key(&mut self, repository: Uuid) -> StoreResult<Option<SealedKey>> {
        let row = sqlx::query!(
            "SELECT kek_sealed, kek_sealed_by FROM repository WHERE id = $1",
            repository
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| SealedKey { sealed_by: r.kek_sealed_by, sealed: r.kek_sealed }))
    }

    async fn workload_key(
        &mut self,
        repository: Uuid,
        workload: Uuid,
    ) -> StoreResult<Option<SealedKey>> {
        let row = sqlx::query!(
            "SELECT dek_sealed FROM workload_key WHERE repository_id = $1 AND workload_id = $2",
            repository,
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| SealedKey {
            sealed_by: format!("repository:{repository}"),
            sealed: r.dek_sealed,
        }))
    }

    async fn keep_workload_key(
        &mut self,
        repository: Uuid,
        workload: Uuid,
        dek: &SealedKey,
        at: SystemTime,
    ) -> StoreResult<SealedKey> {
        sqlx::query!(
            "INSERT INTO workload_key (repository_id, workload_id, dek_sealed, created_at)
             VALUES ($1, $2, $3, $4) ON CONFLICT (repository_id, workload_id) DO NOTHING",
            repository,
            workload,
            dek.sealed,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        self.workload_key(repository, workload)
            .await?
            .ok_or_else(|| StoreError::Corrupt("a workload key kept and not found".into()))
    }

    async fn insert_backup(&mut self, b: &Backup) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO backup (id, repository_id, workload_id, volume_id, plan_id, parent_id,
                                 status, consistent, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            b.id,
            b.repository,
            b.workload,
            b.volume,
            b.plan,
            b.parent,
            b.status.as_str(),
            b.consistent,
            from_time(b.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn finish_backup(&mut self, b: &Backup) -> StoreResult<bool> {
        let s = &b.stats;
        let done = sqlx::query!(
            "UPDATE backup SET status = $2, consistent = $3, files = $4, directories = $5,
                               bytes = $6, chunks_new = $7, bytes_new = $8, stored_bytes = $9,
                               files_unchanged = $10, skipped = $11, error = $12,
                               finished_at = $13
              WHERE id = $1 AND status = 'pending'",
            b.id,
            b.status.as_str(),
            b.consistent,
            signed(s.files),
            signed(s.directories),
            signed(s.bytes),
            signed(s.chunks_new),
            signed(s.bytes_new),
            signed(s.stored_bytes),
            signed(s.files_unchanged),
            &s.skipped,
            b.error,
            b.finished.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
