// SPDX-License-Identifier: AGPL-3.0-or-later

//! What pruning reads and writes (migration 0044): candidates and readers, the access ledger,
//! marks, forgetting, and the sweep's hold on a prefix.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{
    AccessRecord, Area, Prefix, PruneRead, PruneWrite, StoreError, StoreResult,
};
use korpis_core::backup::BackupStatus;
use korpis_core::backup::retention::Candidate;
use korpis_core::extension::AccessMode;
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

impl PruneRead for PgStore {
    async fn prune_candidates(
        &self,
        (repository, workload): Prefix,
    ) -> StoreResult<Vec<Candidate>> {
        let rows = sqlx::query!(
            "SELECT id, volume_id, status, created_at FROM backup
              WHERE repository_id = $1 AND workload_id = $2 AND deleted_at IS NULL
              ORDER BY id",
            repository,
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let status = BackupStatus::parse(&r.status)
                    .ok_or_else(|| StoreError::Corrupt(format!("backup status {}", r.status)))?;
                Ok(Candidate {
                    id: r.id,
                    volume: r.volume_id,
                    created: to_time(r.created_at),
                    status,
                })
            })
            .collect()
    }

    async fn backups_being_read(
        &self,
        (repository, workload): Prefix,
        now: SystemTime,
    ) -> StoreResult<BTreeSet<Uuid>> {
        let ids = sqlx::query_file_scalar!(
            "queries/backups_being_read.sql",
            repository,
            workload,
            from_time(now)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(ids.into_iter().collect())
    }

    async fn writable_until(
        &self,
        (repository, workload): Prefix,
        now: SystemTime,
    ) -> StoreResult<Option<SystemTime>> {
        let until = sqlx::query_scalar!(
            "SELECT max(expires_at) FROM backup_access
              WHERE repository_id = $1 AND workload_id = $2 AND mode = 'write'
                AND area = 'backups' AND expires_at > $3",
            repository,
            workload,
            from_time(now)
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(until.map(to_time))
    }

    async fn streams_granted(
        &self,
        (repository, workload): Prefix,
        node: Uuid,
    ) -> StoreResult<bool> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM backup_access
                    WHERE repository_id = $1 AND workload_id = $2 AND node_id = $3
                      AND area = 'streams' AND mode = 'write'
               ) AS "granted!""#,
            repository,
            workload,
            node
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn marked(
        &self,
        (repository, workload): Prefix,
        plan: Uuid,
        before: SystemTime,
    ) -> StoreResult<Vec<String>> {
        sqlx::query_scalar!(
            "SELECT name FROM prune_mark
              WHERE repository_id = $1 AND workload_id = $2 AND plan_id = $3 AND marked_at <= $4
              ORDER BY name",
            repository,
            workload,
            plan,
            from_time(before)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn forgotten_backups(
        &self,
        (repository, workload): Prefix,
    ) -> StoreResult<BTreeSet<Uuid>> {
        let ids = sqlx::query_scalar!(
            "SELECT id FROM backup
              WHERE repository_id = $1 AND workload_id = $2 AND deleted_at IS NOT NULL",
            repository,
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(ids.into_iter().collect())
    }
}

impl PruneWrite for PgTx {
    async fn record_access(&mut self, a: &AccessRecord) -> StoreResult<()> {
        let mode = match a.mode {
            AccessMode::Read => "read",
            AccessMode::Write => "write",
        };
        let area = match a.area {
            Area::Backups => "backups",
            Area::Streams => "streams",
        };
        sqlx::query!(
            "INSERT INTO backup_access
                 (id, repository_id, workload_id, backup_id, mode, granted_at, expires_at, area,
                  node_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            Uuid::now_v7(),
            a.prefix.0,
            a.prefix.1,
            a.backup,
            mode,
            from_time(a.granted_at),
            from_time(a.expires_at),
            area,
            a.node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn forget_backups(&mut self, backups: &[Uuid], at: SystemTime) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE backup SET deleted_at = $2 WHERE id = ANY($1) AND deleted_at IS NULL",
            backups,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn mark_unreferenced(
        &mut self,
        ((repository, workload), plan): (Prefix, Uuid),
        names: &[String],
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO prune_mark (repository_id, workload_id, name, plan_id, marked_at)
             SELECT $1, $2, name, $4, $5 FROM unnest($3::text[]) AS name
             ON CONFLICT (repository_id, workload_id, name) DO NOTHING",
            repository,
            workload,
            names,
            plan,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn unmark(
        &mut self,
        (repository, workload): Prefix,
        names: &[String],
    ) -> StoreResult<()> {
        sqlx::query!(
            "DELETE FROM prune_mark
              WHERE repository_id = $1 AND workload_id = $2 AND name = ANY($3)",
            repository,
            workload,
            names
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn hold_for_sweep(
        &mut self,
        (repository, workload): Prefix,
        job: Uuid,
    ) -> StoreResult<bool> {
        sqlx::query!(
            "SELECT 1 AS one FROM workload_key
              WHERE repository_id = $1 AND workload_id = $2 FOR UPDATE",
            repository,
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let held = sqlx::query_file!("queries/hold_for_sweep.sql", repository, workload, job)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(held.rows_affected() == 1)
    }

    async fn release_sweep(
        &mut self,
        (repository, workload): Prefix,
        job: Uuid,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE workload_key SET sweeping_job = NULL
              WHERE repository_id = $1 AND workload_id = $2 AND sweeping_job = $3",
            repository,
            workload,
            job
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn swept(&mut self, (repository, workload): Prefix) -> StoreResult<bool> {
        let holder = sqlx::query_scalar!(
            "SELECT sweeping_job FROM workload_key
              WHERE repository_id = $1 AND workload_id = $2 FOR SHARE",
            repository,
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(holder.flatten().is_some())
    }

    async fn drop_ended_access(&mut self, before: SystemTime) -> StoreResult<u64> {
        let gone =
            sqlx::query!("DELETE FROM backup_access WHERE expires_at < $1", from_time(before))
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(gone.rows_affected())
    }
}
