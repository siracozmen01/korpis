// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans a restore recovers from what nodes present (§8 of docs/03-state.md): a plan in flight
//! the store lacks, recorded as applying and recovered, or one it holds as approved or queued,
//! started; and any other plan of the workload the store holds as applying, cancelled, since the
//! node has moved past it.

use crate::PgTx;
use crate::errors::from_sqlx;
use korpis_app::ports::{PlanRecovery, StoreResult, WorkloadWrite as _};
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::{Plan, PlanTarget};
use uuid::Uuid;

impl PgTx {
    /// What [`korpis_app::ports::RecoveryWrite::recover_plan`] does.
    pub(super) async fn recover_plan_row(&mut self, plan: &Plan) -> StoreResult<PlanRecovery> {
        let held = sqlx::query_scalar!("SELECT status FROM plan WHERE id = $1", plan.id)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        match held.as_deref() {
            Some("approved" | "queued") => {
                let closed = self.close_applying(plan).await?;
                sqlx::query!("UPDATE plan SET status = 'applying' WHERE id = $1", plan.id)
                    .execute(&mut *self.tx)
                    .await
                    .map_err(from_sqlx)?;
                Ok(PlanRecovery::Started { closed })
            }
            Some(_) => Ok(PlanRecovery::Held),
            None => {
                if let Some(missing) = self.unrecoverable(plan).await? {
                    return Ok(PlanRecovery::Refused { missing });
                }
                let proposed_under = match plan.proposed_under {
                    Some(g) => self.held("grants", g).await?.then_some(g),
                    None => None,
                };
                let from_intent = match plan.from_intent {
                    Some(i) => self.held("intent", i).await?.then_some(i),
                    None => None,
                };
                let recovered = Plan {
                    status: PlanStatus::Applying,
                    completed: None,
                    proposed_under,
                    from_intent,
                    ..plan.clone()
                };
                let closed = self.close_applying(plan).await?;
                self.insert_plan(&recovered).await?;
                sqlx::query!("UPDATE plan SET recovered = true WHERE id = $1", plan.id)
                    .execute(&mut *self.tx)
                    .await
                    .map_err(from_sqlx)?;
                Ok(PlanRecovery::Recorded { closed })
            }
        }
    }

    /// Cancels every plan of `plan`'s workload but `plan` that the store holds as applying.
    async fn close_applying(&mut self, plan: &Plan) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "UPDATE plan SET status = 'cancelled'
              WHERE workload_id = $1 AND status = 'applying' AND id <> $2
             RETURNING id",
            plan.workload,
            plan.id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)
    }

    /// What `plan` names that a plan row cannot be written without and the store does not hold.
    async fn unrecoverable(&mut self, plan: &Plan) -> StoreResult<Option<&'static str>> {
        if let PlanTarget::Intent(i) = plan.target
            && !self.held("intent", i).await?
        {
            return Ok(Some("intent"));
        }
        if let Some(v) = plan.target.volume()
            && !self.held("volume", v).await?
        {
            return Ok(Some("volume"));
        }
        if let Some(r) = plan.target.repository()
            && !self.held("repository", r).await?
        {
            return Ok(Some("repository"));
        }
        Ok(None)
    }

    /// Whether `table` holds a row with identifier `id`.
    async fn held(&mut self, table: &'static str, id: Uuid) -> StoreResult<bool> {
        // A table named by this file, never by input.
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)");
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)
    }
}
