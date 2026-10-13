// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared sets (§3.3 of docs/01-model.md). Drift is not stored: it is the head intent's
//! `managed_by` read against the workload's `set_id`.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, principal, to_count, to_time};
use crate::tenancy::name;
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{SetRead, SetWrite, StoreError, StoreResult};
use korpis_core::workload::set::{DeclaredSet, DriftMode};
use uuid::Uuid;

struct SetRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    mode: String,
    version: i64,
    created_by: String,
    created_at: DateTime<Utc>,
}

impl SetRow {
    fn into_domain(self) -> StoreResult<DeclaredSet> {
        Ok(DeclaredSet {
            id: self.id,
            project: self.project_id,
            name: name(self.name)?,
            mode: DriftMode::parse(&self.mode)
                .ok_or_else(|| StoreError::Corrupt(format!("set mode {}", self.mode)))?,
            version: count("set version", self.version)?,
            created_by: principal(&self.created_by)?,
            created_at: to_time(self.created_at),
        })
    }
}

impl SetRead for PgStore {
    async fn declared_set(&self, project: Uuid, name: &str) -> StoreResult<Option<DeclaredSet>> {
        let row = sqlx::query_as!(
            SetRow,
            "SELECT id, project_id, name, mode, version, created_by, created_at
               FROM declared_set WHERE project_id = $1 AND name = $2",
            project,
            name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(SetRow::into_domain).transpose()
    }

    async fn declared_set_by_id(&self, id: Uuid) -> StoreResult<Option<DeclaredSet>> {
        let row = sqlx::query_as!(
            SetRow,
            "SELECT id, project_id, name, mode, version, created_by, created_at
               FROM declared_set WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(SetRow::into_domain).transpose()
    }

    async fn declared_sets(&self, project: Uuid, page: Page) -> StoreResult<Vec<DeclaredSet>> {
        let rows = sqlx::query_as!(
            SetRow,
            "SELECT id, project_id, name, mode, version, created_by, created_at
               FROM declared_set
              WHERE project_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            project,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(SetRow::into_domain).collect()
    }

    async fn set_intent(&self, workload: Uuid, set: Uuid) -> StoreResult<Option<Uuid>> {
        sqlx::query_scalar!(
            "SELECT id FROM intent WHERE workload_id = $1 AND managed_by = $2
              ORDER BY version DESC LIMIT 1",
            workload,
            set
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)
    }
}

impl SetWrite for PgTx {
    async fn insert_declared_set(&mut self, s: &DeclaredSet) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO declared_set (id, project_id, name, mode, version, created_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            s.id,
            s.project,
            s.name.as_str(),
            s.mode.as_str(),
            to_count("set version", s.version)?,
            s.created_by.key(),
            from_time(s.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn set_drift_mode(
        &mut self,
        set: Uuid,
        mode: DriftMode,
        expected: u64,
    ) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE declared_set SET mode = $2, version = version + 1
              WHERE id = $1 AND version = $3",
            set,
            mode.as_str(),
            to_count("set version", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn join_set(&mut self, workload: Uuid, set: Uuid) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE workload SET set_id = $2 WHERE id = $1 AND set_id IS NULL",
            workload,
            set
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }
}
