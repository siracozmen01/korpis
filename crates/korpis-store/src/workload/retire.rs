// SPDX-License-Identifier: AGPL-3.0-or-later

//! Retiring workloads (§3.3 of docs/01-model.md).

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{RetirementWrite, StoreResult};
use korpis_core::workload::Plan;
use std::time::SystemTime;
use uuid::Uuid;

impl RetirementWrite for PgTx {
    async fn retiring(&mut self, workload: Uuid) -> StoreResult<bool> {
        sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM plan WHERE workload_id = $1 AND kind = 'retire'
                              AND status IN ('approved', 'queued', 'applying')) AS \"retiring!\"",
            workload
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)
    }

    async fn supersede_all_proposed(&mut self, keep: &Plan) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE plan SET status = 'superseded'
              WHERE workload_id = $1 AND id <> $2 AND status = 'proposed'",
            keep.workload,
            keep.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn retire_workload(&mut self, workload: Uuid, at: SystemTime) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE workload SET deleted_at = $2, version = version + 1
              WHERE id = $1 AND deleted_at IS NULL",
            workload,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
