// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing workloads: creation, the head and target compare-and-swaps, power, and plans.

use super::rows::{PlanRow, WorkloadRow, approval_columns, classification_doc, desired_str};
use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{ImpactDoc, StepDoc, from_time, to_count, to_json};
use korpis_app::ports::{StoreError, StoreResult, WorkloadWrite};
use korpis_core::workload::plan::{Override, PlanStatus};
use korpis_core::workload::{Intent, Plan, PlanTarget, PowerRecord, Workload};
use std::time::SystemTime;
use uuid::Uuid;

fn encoded(tx: &PgTx, intent: &Intent) -> StoreResult<String> {
    tx.bodies.encode(&intent.body).map_err(|e| StoreError::Failed(e.to_string()))
}

impl PgTx {
    async fn insert_intent(&mut self, i: &Intent) -> StoreResult<()> {
        let body = encoded(self, i)?;
        sqlx::query!(
            "INSERT INTO intent (id, workload_id, version, parent_id, declared_by, declared_at,
                                 body, schema_version, managed_by)
             VALUES ($1, $2, $3, $4, $5, $6, ($7::text)::jsonb, $8, $9)",
            i.id,
            i.workload,
            to_count("intent version", i.version)?,
            i.parent,
            i.declared_by.key(),
            from_time(i.declared_at),
            body,
            self.bodies.schema_version(),
            i.managed_by
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn insert_power(&mut self, p: &PowerRecord) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO power (workload_id, version, desired, cycle, set_by, set_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            p.workload,
            to_count("power version", p.power.version)?,
            desired_str(p.power.desired),
            to_count("cycle", p.power.cycle)?,
            p.set_by.key(),
            from_time(p.set_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}

impl WorkloadWrite for PgTx {
    async fn intent_within(&mut self, id: Uuid) -> StoreResult<Option<Intent>> {
        super::intent(&mut *self.tx, self.bodies.as_ref(), id).await
    }

    async fn lock_workload(&mut self, id: Uuid) -> StoreResult<Option<Workload>> {
        let row = sqlx::query_as!(
            WorkloadRow,
            "SELECT id, project_id, organization_id, name, labels, intent_id, intent_version,
                    target_id, target_version, power_version, power_target_version, version,
                    created, deleted_at, set_id, classification, unreproducible_at,
                    unreproducible_reason
               FROM workload WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(WorkloadRow::into_domain).transpose()
    }

    async fn create_workload(
        &mut self,
        w: &Workload,
        intent: &Intent,
        power: &PowerRecord,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO workload (id, project_id, organization_id, name, labels, created, set_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            w.id,
            w.project,
            w.organization,
            w.name.as_str(),
            to_json("labels", w.labels.as_map())?,
            from_time(w.created),
            w.set
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        self.insert_intent(intent).await?;
        self.insert_power(power).await?;
        sqlx::query!(
            "UPDATE workload SET intent_id = $2, intent_version = $3, power_version = $4,
                                 version_floor = greatest(version_floor, $3, $4)
              WHERE id = $1",
            w.id,
            intent.id,
            to_count("intent version", intent.version)?,
            to_count("power version", power.power.version)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn append_intent(&mut self, intent: &Intent) -> StoreResult<bool> {
        let version = to_count("intent version", intent.version)?;
        // Check the head and lock the row first: inserting a duplicate version would abort the
        // transaction instead of reporting the conflict.
        let head = sqlx::query_scalar!(
            "SELECT id FROM workload
              WHERE id = $1 AND intent_version = $2::bigint - 1 FOR UPDATE",
            intent.workload,
            version
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if head.is_none() {
            return Ok(false);
        }
        self.insert_intent(intent).await?;
        sqlx::query!(
            "UPDATE workload SET intent_id = $2, intent_version = $3,
                                 version_floor = greatest(version_floor, $3)
              WHERE id = $1",
            intent.workload,
            intent.id,
            version
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(true)
    }

    async fn append_power(&mut self, p: &PowerRecord) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE workload SET power_version = $2,
                                 version_floor = greatest(version_floor, $2)
              WHERE id = $1 AND power_version = $2::bigint - 1",
            p.workload,
            to_count("power version", p.power.version)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if moved.rows_affected() != 1 {
            return Ok(false);
        }
        self.insert_power(p).await?;
        Ok(true)
    }

    async fn plan_applying(&mut self, workload: Uuid) -> StoreResult<bool> {
        let applying = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM plan WHERE workload_id = $1 AND status = 'applying')
                 AS \"applying!\"",
            workload
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(applying)
    }

    async fn made_and_settled(&mut self, workload: Uuid) -> StoreResult<bool> {
        sqlx::query_file_scalar!("queries/plan_made_and_settled.sql", workload)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)
    }

    async fn insert_plan(&mut self, p: &Plan) -> StoreResult<()> {
        let (to_intent, to_power) = match p.target {
            PlanTarget::Intent(i) => (Some(i), None),
            PlanTarget::Power(v) => (None, Some(to_count("to_power", v)?)),
            PlanTarget::Retire
            | PlanTarget::Migrate
            | PlanTarget::Remake
            | PlanTarget::Volume(..)
            | PlanTarget::Prune(_) => (None, None),
        };
        let to_volume = p.target.volume();
        let steps: Vec<StepDoc> = p.steps.iter().map(StepDoc::from_domain).collect();
        let (approval, scope) = approval_columns(&p.approval);
        sqlx::query_file!(
            "queries/plan_insert.sql",
            p.id,
            p.workload,
            p.target.kind(),
            p.operation,
            p.from_intent,
            to_count("from_power", p.from_power)?,
            to_intent,
            to_power,
            from_time(p.computed_at),
            p.computed_by.key(),
            to_json("plan steps", &steps)?,
            to_json("plan impact", &ImpactDoc::from_domain(&p.impact))?,
            p.status.as_str(),
            from_time(p.expires_at),
            p.proposed_under,
            approval,
            scope.as_ref().map(|s| to_json("approval_scope", s)).transpose()?,
            p.approval_reason,
            to_volume,
            p.target.repository()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        self.update_plan(p, p.status).await.map(|_| ())
    }

    async fn lock_plan(&mut self, id: Uuid) -> StoreResult<Option<Plan>> {
        let row = sqlx::query_file_as!(PlanRow, "queries/plan_for_update.sql", id)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        row.map(PlanRow::into_domain).transpose()
    }

    async fn update_plan(&mut self, p: &Plan, from: PlanStatus) -> StoreResult<bool> {
        let completed = p
            .completed
            .map(|c| i32::try_from(c).map_err(|_| StoreError::Corrupt("completed".into())))
            .transpose()?;
        let updated = sqlx::query!(
            "UPDATE plan SET status = $3, completed = $4, apply_after = $5, approved_by = $6,
                             approved_at = $7
              WHERE id = $1 AND status = $2",
            p.id,
            from.as_str(),
            p.status.as_str(),
            completed,
            p.apply_after.map(from_time),
            p.approved_by.as_ref().map(korpis_core::principal::Principal::key),
            p.approved_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(updated.rows_affected() == 1)
    }

    async fn supersede_proposed(&mut self, keep: &Plan) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE plan SET status = 'superseded'
              WHERE workload_id = $1 AND id <> $2 AND kind = $3 AND status = 'proposed'",
            keep.workload,
            keep.id,
            keep.target.kind()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn mark_unreproducible(
        &mut self,
        workload: Uuid,
        at: SystemTime,
        reason: &str,
    ) -> StoreResult<bool> {
        let marked = sqlx::query!(
            "UPDATE workload SET unreproducible_at = $2, unreproducible_reason = $3
              WHERE id = $1 AND unreproducible_at IS NULL",
            workload,
            from_time(at),
            reason
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(marked.rows_affected() == 1)
    }

    async fn set_classification(
        &mut self,
        workload: Uuid,
        overrides: &[Override],
        expected: u64,
    ) -> StoreResult<bool> {
        let set = sqlx::query!(
            "UPDATE workload SET classification = $2, version = version + 1
              WHERE id = $1 AND version = $3 AND deleted_at IS NULL",
            workload,
            to_json("workload classification", &classification_doc(overrides))?,
            to_count("workload version", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(set.rows_affected() == 1)
    }

    async fn move_target(&mut self, workload: Uuid, to: Uuid, expected: u64) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE workload SET target_id = $2, target_version = target_version + 1,
                    power_target_version = CASE WHEN target_id IS NULL AND power_target_version = 0
                                                THEN 1 ELSE power_target_version END
              WHERE id = $1 AND target_version = $3 AND intent_id = $2",
            workload,
            to,
            to_count("target_version", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(moved.rows_affected() == 1)
    }

    async fn move_power_target(
        &mut self,
        workload: Uuid,
        to: u64,
        expected: u64,
    ) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE workload SET power_target_version = $2
              WHERE id = $1 AND power_target_version = $3 AND power_version = $2",
            workload,
            to_count("power target", to)?,
            to_count("power target", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(moved.rows_affected() == 1)
    }

    async fn restore_targets(
        &mut self,
        workload: Uuid,
        to: Option<Uuid>,
        power: u64,
        expected: u64,
    ) -> StoreResult<bool> {
        let restored = sqlx::query!(
            "UPDATE workload SET target_id = $2, target_version = target_version + 1,
                                 power_target_version = $3
              WHERE id = $1 AND target_version = $4",
            workload,
            to,
            to_count("power target", power)?,
            to_count("target_version", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(restored.rows_affected() == 1)
    }
}
