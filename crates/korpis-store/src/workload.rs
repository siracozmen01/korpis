// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads, intents, power declarations, and plans (§3 and §7 of docs/03-state.md).

mod retire;
pub(crate) mod rows;
mod write;

use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::Page;
use korpis_app::ports::{StoreResult, WorkloadFilter, WorkloadRead};
use korpis_core::workload::{Intent, Plan, PowerRecord, Workload};
use rows::{IntentRow, PlanRow, PowerRow, WorkloadRow};
use uuid::Uuid;

impl WorkloadRead for PgStore {
    async fn workload(&self, id: Uuid) -> StoreResult<Option<Workload>> {
        let row = sqlx::query_as!(
            WorkloadRow,
            "SELECT id, project_id, organization_id, name, labels, intent_id, intent_version,
                    target_id, target_version, power_version, power_target_version, version,
                    created, deleted_at, set_id, classification
               FROM workload WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(WorkloadRow::into_domain).transpose()
    }

    async fn workloads(&self, f: &WorkloadFilter, page: Page) -> StoreResult<Vec<Workload>> {
        let rows = sqlx::query_as!(
            WorkloadRow,
            "SELECT id, project_id, organization_id, name, labels, intent_id, intent_version,
                    target_id, target_version, power_version, power_target_version, version,
                    created, deleted_at, set_id, classification
               FROM workload
              WHERE deleted_at IS NULL
                AND ($1::uuid IS NULL OR project_id = $1)
                AND ($2::uuid IS NULL OR organization_id = $2)
                AND ($3::uuid IS NULL OR id > $3)
                AND ($5::uuid IS NULL OR set_id = $5)
              ORDER BY id LIMIT $4",
            f.project,
            f.organization,
            page.after,
            i64::from(page.size),
            f.set
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(WorkloadRow::into_domain).collect()
    }

    async fn intent(&self, id: Uuid) -> StoreResult<Option<Intent>> {
        let row = sqlx::query_as!(
            IntentRow,
            r#"SELECT id, workload_id, version, parent_id, declared_by, declared_at,
                      body::text AS "body", schema_version, managed_by
                 FROM intent WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| r.into_domain(self.bodies.as_ref())).transpose()
    }

    async fn intents(&self, workload: Uuid, page: Page) -> StoreResult<Vec<Intent>> {
        let rows = sqlx::query_as!(
            IntentRow,
            r#"SELECT id, workload_id, version, parent_id, declared_by, declared_at,
                      body::text AS "body", schema_version, managed_by
                 FROM intent
                WHERE workload_id = $1 AND ($2::uuid IS NULL OR id > $2)
                ORDER BY id LIMIT $3"#,
            workload,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(|r| r.into_domain(self.bodies.as_ref())).collect()
    }

    async fn power(&self, workload: Uuid) -> StoreResult<Option<PowerRecord>> {
        let row = sqlx::query_as!(
            PowerRow,
            "SELECT workload_id, version, desired, cycle, set_by, set_at
               FROM power WHERE workload_id = $1 ORDER BY version DESC LIMIT 1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(PowerRow::into_domain).transpose()
    }

    async fn power_at(&self, workload: Uuid, version: u64) -> StoreResult<Option<PowerRecord>> {
        let row = sqlx::query_as!(
            PowerRow,
            "SELECT workload_id, version, desired, cycle, set_by, set_at
               FROM power WHERE workload_id = $1 AND version = $2",
            workload,
            crate::stored::to_count("power version", version)?
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(PowerRow::into_domain).transpose()
    }

    async fn plan(&self, id: Uuid) -> StoreResult<Option<Plan>> {
        let row = sqlx::query_file_as!(PlanRow, "queries/plan_by_id.sql", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(PlanRow::into_domain).transpose()
    }

    async fn governing_grant(&self, workload: Uuid) -> StoreResult<Option<Uuid>> {
        let row = sqlx::query_file_scalar!("queries/plan_governing_grant.sql", workload)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        Ok(row.flatten())
    }

    async fn open_plans(&self, workload: Uuid) -> StoreResult<Vec<Plan>> {
        let rows = sqlx::query_file_as!(PlanRow, "queries/plans_open.sql", workload)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter().map(PlanRow::into_domain).collect()
    }

    async fn plans(&self, workload: Uuid, page: Page) -> StoreResult<Vec<Plan>> {
        let rows = sqlx::query_file_as!(
            PlanRow,
            "queries/plans_of_workload.sql",
            workload,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(PlanRow::into_domain).collect()
    }
}
