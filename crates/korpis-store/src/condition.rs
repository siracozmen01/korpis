// SPDX-License-Identifier: AGPL-3.0-or-later

//! Conditions as events (§5 of docs/15-observability.md, migration 0024).

use crate::errors::from_sqlx;
use crate::stored::from_time;
use crate::{PgStore, PgTx};
use korpis_app::ports::{ConditionRead, ConditionWrite, StoreError, StoreResult};
use korpis_core::event::Condition;
use korpis_core::workload::status::Convergence;
use std::time::SystemTime;
use uuid::Uuid;

impl ConditionRead for PgStore {
    async fn decided_convergence(&self, workload: Uuid) -> StoreResult<Option<Convergence>> {
        let found = sqlx::query_scalar!(
            "SELECT convergence FROM workload_condition WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        found
            .map(|c| {
                Convergence::parse(&c)
                    .ok_or_else(|| StoreError::Corrupt(format!("convergence {c:?}")))
            })
            .transpose()
    }
}

impl ConditionWrite for PgTx {
    async fn append_condition(&mut self, c: &Condition) -> StoreResult<()> {
        let (from, to) = c.change.names();
        let detail = serde_json::json!({ "from": from, "to": to });
        sqlx::query!(
            "SELECT korpis_append_event('workload:' || w.id, $2, $3, $4, w.organization_id,
                                        w.id, NULL, $5)
               FROM workload w WHERE w.id = $1",
            c.workload,
            from_time(c.at),
            c.change.kind(),
            c.change.severity().as_str(),
            detail
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn swap_convergence(
        &mut self,
        workload: Uuid,
        expected: Option<Convergence>,
        to: Convergence,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let n = match expected {
            None => {
                sqlx::query!(
                    "INSERT INTO workload_condition (workload_id, convergence, at)
                 VALUES ($1, $2, $3) ON CONFLICT (workload_id) DO NOTHING",
                    workload,
                    to.as_str(),
                    from_time(at)
                )
                .execute(&mut *self.tx)
                .await
            }
            Some(e) => {
                sqlx::query!(
                    "UPDATE workload_condition SET convergence = $3, at = $4
                  WHERE workload_id = $1 AND convergence = $2",
                    workload,
                    e.as_str(),
                    to.as_str(),
                    from_time(at)
                )
                .execute(&mut *self.tx)
                .await
            }
        }
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }
}
