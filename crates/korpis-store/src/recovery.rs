// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a restore recovers from the assignments nodes present (§8 of docs/03-state.md), written
//! as recovered: `recovered_from` on the intent and the power declaration names the node, the key,
//! and the signature.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_count, to_json};
use chrono::{DateTime, Utc};
use korpis_app::ports::{
    Advance, Advanced, IntentRecovery, RecoveredFrom, RecoveryWrite, StoreError, StoreResult,
};
use korpis_core::workload::{Intent, PowerRecord, PowerState, Workload};
use std::fmt::Write as _;
use uuid::Uuid;

/// `recovered_from`, as the store keeps it.
#[derive(serde::Serialize)]
struct RecoveredDoc {
    node: Uuid,
    key: String,
    signature: String,
    generation: u64,
    at: DateTime<Utc>,
}

fn recovered_doc(from: &RecoveredFrom) -> StoreResult<serde_json::Value> {
    let mut signature = String::with_capacity(from.signature.len() * 2);
    for b in &from.signature {
        let _ = write!(signature, "{b:02x}");
    }
    let doc = RecoveredDoc {
        node: from.node,
        key: from.key_id.clone(),
        signature,
        generation: from.generation,
        at: from_time(from.at),
    };
    to_json("recovered_from", &doc)
}

impl RecoveryWrite for PgTx {
    async fn recover_workload(&mut self, w: &Workload) -> StoreResult<bool> {
        let done = sqlx::query!(
            "INSERT INTO workload (id, project_id, organization_id, name, labels, created)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT DO NOTHING",
            w.id,
            w.project,
            w.organization,
            w.name.as_str(),
            to_json("labels", w.labels.as_map())?,
            from_time(w.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn recover_intent(
        &mut self,
        i: &Intent,
        from: &RecoveredFrom,
    ) -> StoreResult<IntentRecovery> {
        let version = to_count("intent version", i.version)?;
        let held = sqlx::query_scalar!(
            "SELECT id FROM intent WHERE id = $1 OR (workload_id = $2 AND version = $3)",
            i.id,
            i.workload,
            version
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if held.contains(&i.id) {
            return Ok(IntentRecovery::Held);
        }
        if !held.is_empty() {
            return Ok(IntentRecovery::VersionTaken);
        }
        let body = self.bodies.encode(&i.body).map_err(|e| StoreError::Failed(e.to_string()))?;
        sqlx::query!(
            "INSERT INTO intent (id, workload_id, version, parent_id, declared_by, declared_at,
                                 body, schema_version, managed_by, recovered_from)
             VALUES ($1, $2, $3, (SELECT id FROM intent WHERE id = $4), $5, $6,
                     ($7::text)::jsonb, $8, (SELECT id FROM declared_set WHERE id = $9), $10)",
            i.id,
            i.workload,
            version,
            i.parent,
            i.declared_by.key(),
            from_time(i.declared_at),
            body,
            self.bodies.schema_version(),
            i.managed_by,
            recovered_doc(from)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(IntentRecovery::Recorded)
    }

    async fn recover_power(&mut self, p: &PowerRecord, from: &RecoveredFrom) -> StoreResult<bool> {
        let desired = match p.power.desired {
            PowerState::Running => "running",
            PowerState::Stopped => "stopped",
        };
        let done = sqlx::query!(
            "INSERT INTO power (workload_id, version, desired, cycle, set_by, set_at,
                                recovered_from)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (workload_id, version) DO NOTHING",
            p.workload,
            to_count("power version", p.power.version)?,
            desired,
            to_count("cycle", p.power.cycle)?,
            p.set_by.key(),
            from_time(p.set_at),
            recovered_doc(from)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn advance_recovered(&mut self, a: Advance) -> StoreResult<Advanced> {
        let (target, target_version) = a.target;
        let target_version = to_count("intent version", target_version)?;
        let power = to_count("power version", a.power)?;
        let held = sqlx::query!(
            r#"SELECT w.intent_version, w.power_target_version,
                      (SELECT version FROM intent WHERE id = w.target_id) AS "target_version?"
                 FROM workload w WHERE w.id = $1 FOR UPDATE"#,
            a.workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .ok_or_else(|| StoreError::Failed(format!("no workload {}", a.workload)))?;
        let moved = Advanced {
            head: target_version > held.intent_version,
            target: held.target_version.is_none_or(|v| target_version > v),
            power: power > held.power_target_version,
        };
        sqlx::query!(
            "UPDATE workload
                SET intent_id = CASE WHEN $4 THEN $2 ELSE intent_id END,
                    intent_version = CASE WHEN $4 THEN $3 ELSE intent_version END,
                    target_id = CASE WHEN $5 THEN $2 ELSE target_id END,
                    target_version = CASE WHEN $5 THEN target_version + 1 ELSE target_version END,
                    power_version = greatest(power_version, $6),
                    power_target_version = greatest(power_target_version, $6),
                    version_floor = greatest(version_floor, $3, $6)
              WHERE id = $1",
            a.workload,
            target,
            target_version,
            moved.head,
            moved.target,
            power
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(moved)
    }
}
