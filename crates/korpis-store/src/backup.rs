// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories, their sealed keys, and backups (§5 of docs/06-storage.md).

mod rows;

use crate::errors::from_sqlx;
use crate::stored::from_time;
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{BackupRead, BackupWrite, SealedKey, StoreError, StoreResult};
use korpis_core::backup::{Backup, Repository};
use rows::{BackupRow, RepositoryRow, signed};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

impl BackupRead for PgStore {
    async fn repository(&self, id: Uuid) -> StoreResult<Option<Repository>> {
        let row = sqlx::query_as!(
            RepositoryRow,
            "SELECT id, organization_id, project_id, name, provider_name, prefix, key_custody,
                    keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly,
                    keep_failed_s, min_retention_s, streams, created_at, deleted_at
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
                    keep_failed_s, min_retention_s, streams, created_at, deleted_at
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

    async fn offload_repository(
        &self,
        organization: Uuid,
        project: Uuid,
    ) -> StoreResult<Option<Repository>> {
        let row = sqlx::query_as!(
            RepositoryRow,
            "SELECT id, organization_id, project_id, name, provider_name, prefix, key_custody,
                    keep_last, keep_hourly, keep_daily, keep_weekly, keep_monthly, keep_yearly,
                    keep_failed_s, min_retention_s, streams, created_at, deleted_at
               FROM repository
              WHERE organization_id = $1 AND streams AND deleted_at IS NULL
                AND (project_id = $2 OR project_id IS NULL)
              ORDER BY project_id NULLS LAST LIMIT 1",
            organization,
            project
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(RepositoryRow::into_domain).transpose()
    }

    async fn streams_taken(&self, organization: Uuid, project: Option<Uuid>) -> StoreResult<bool> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM repository
                    WHERE organization_id = $1 AND project_id IS NOT DISTINCT FROM $2
                      AND streams AND deleted_at IS NULL
               ) AS "taken!""#,
            organization,
            project
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn backup_open(&self, repository: Uuid, workload: Uuid) -> StoreResult<bool> {
        let open = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM backup b JOIN plan p ON p.id = b.plan_id
                    WHERE b.repository_id = $1 AND b.workload_id = $2 AND b.status = 'pending'
                      AND p.status IN ('proposed', 'approved', 'queued', 'applying')
               ) AS "open!""#,
            repository,
            workload
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(open)
    }

    async fn last_backup(&self, repository: Uuid, volume: Uuid) -> StoreResult<Option<Uuid>> {
        let id = sqlx::query_scalar!(
            "SELECT id FROM backup
              WHERE repository_id = $1 AND volume_id = $2 AND status = 'succeeded'
                AND deleted_at IS NULL
              ORDER BY id DESC LIMIT 1",
            repository,
            volume
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(id)
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
        sqlx::query!("SELECT pg_advisory_xact_lock(hashtext('repository:' || $1))", r.provider)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let t = &r.retention;
        let n = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let s = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        sqlx::query!(
            "INSERT INTO repository (id, organization_id, project_id, name, provider_name, prefix,
                                     key_custody, kek_sealed, kek_sealed_by, keep_last,
                                     keep_hourly, keep_daily, keep_weekly, keep_monthly,
                                     keep_yearly, keep_failed_s, min_retention_s, streams,
                                     created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18, $19)",
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
            r.streams,
            from_time(r.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        // After the insert, so that a name taken is told as that rather than as an overlap.
        let overlaps = sqlx::query_file_scalar!(
            "queries/repository_overlaps.sql",
            r.organization,
            r.provider,
            r.prefix,
            r.id
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if overlaps {
            return Err(StoreError::Duplicate(korpis_app::ports::Unique::RepositoryPrefix));
        }
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
