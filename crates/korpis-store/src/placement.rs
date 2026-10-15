// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placements (§3.4 of docs/01-model.md, §2.3 of docs/05-scheduling.md).

use crate::errors::from_sqlx;
use crate::node::NodeRow;
use crate::stored::{
    ExplanationDoc, bounds, bounds_doc, count, from_json, from_time, to_json, to_time,
};
use crate::tenancy::labels;
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{PlacementRead, PlacementWrite, StoreError, StoreResult};
use korpis_core::node::Node;
use korpis_core::placement::{BoundBy, Placement, Unplaced};
use korpis_core::schedule::Peer;
use korpis_core::tenancy::quota::{Bounds, Dimension};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

/// The advisory lock every bind and every cordon holds: one key, for the installation.
const PLACEMENT_LOCK: i64 = 0x6b6f_7270_6973_0001;

struct PlacementRow {
    workload_id: Uuid,
    node_id: Uuid,
    intent_id: Uuid,
    reserved: Value,
    bound_at: DateTime<Utc>,
    bound_by: String,
    reason: String,
    explanation: Value,
    authoritative: bool,
}

impl PlacementRow {
    fn into_domain(self) -> StoreResult<Placement> {
        let doc: ExplanationDoc = from_json("placement explanation", self.explanation)?;
        Ok(Placement {
            workload: self.workload_id,
            node: self.node_id,
            intent: self.intent_id,
            reserved: bounds("placement reserved", self.reserved)?,
            bound_at: to_time(self.bound_at),
            bound_by: BoundBy::parse(&self.bound_by).ok_or_else(|| {
                StoreError::Corrupt(format!("placement bound_by {:?}", self.bound_by))
            })?,
            reason: self.reason,
            explanation: doc.into_domain()?,
            authoritative: self.authoritative,
        })
    }
}

impl PlacementRead for PgStore {
    async fn placement(&self, workload: Uuid) -> StoreResult<Option<Placement>> {
        let row = sqlx::query_file_as!(PlacementRow, "queries/placement_of.sql", workload)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(PlacementRow::into_domain).transpose()
    }

    async fn unplaced(&self, workload: Uuid) -> StoreResult<Option<Unplaced>> {
        let row = sqlx::query!(
            "SELECT workload_id, intent_id, since, explanation
               FROM placement_failure WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let doc: ExplanationDoc = from_json("unplaced explanation", r.explanation)?;
            Ok(Unplaced {
                workload: r.workload_id,
                intent: r.intent_id,
                since: to_time(r.since),
                explanation: doc.into_domain()?,
            })
        })
        .transpose()
    }

    async fn awaiting_placement(&self, page: Page) -> StoreResult<Vec<Uuid>> {
        let size = i64::from(page.size);
        sqlx::query_file_scalar!("queries/placement_awaiting.sql", page.after, size)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)
    }
}

impl PlacementWrite for PgTx {
    async fn decision_grant(&mut self, workload: Uuid) -> StoreResult<Option<Uuid>> {
        let row = sqlx::query_file_scalar!("queries/plan_decision_grant.sql", workload)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(row.flatten())
    }

    async fn lock_placement(&mut self) -> StoreResult<()> {
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", PLACEMENT_LOCK)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn allocations(&mut self, nodes: &[Uuid]) -> StoreResult<BTreeMap<Uuid, Bounds>> {
        let rows = sqlx::query_file!("queries/node_allocations.sql", nodes)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let mut out: BTreeMap<Uuid, Bounds> = BTreeMap::new();
        for r in rows {
            let d = Dimension::parse(&r.dimension).ok_or_else(|| {
                StoreError::Corrupt(format!("placement reserved: dimension {:?}", r.dimension))
            })?;
            out.entry(r.node_id).or_default().insert(d, count("node allocated", r.total)?);
        }
        Ok(out)
    }

    async fn serving_nodes(&mut self, owners: &[Uuid]) -> StoreResult<Vec<Node>> {
        let rows = sqlx::query_file_as!(NodeRow, "queries/nodes_serving.sql", owners)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter().map(NodeRow::into_domain).collect()
    }

    async fn peers(&mut self, organization: Uuid, except: Uuid) -> StoreResult<Vec<Peer>> {
        let rows = sqlx::query_file!("queries/placement_peers.sql", organization, except)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let labels = labels(r.labels)?.into_map();
                Ok(Peer { workload: r.workload, project: r.project, node: r.node, labels })
            })
            .collect()
    }

    async fn placement_in(&mut self, workload: Uuid) -> StoreResult<Option<Placement>> {
        let row = sqlx::query_file_as!(PlacementRow, "queries/placement_of.sql", workload)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        row.map(PlacementRow::into_domain).transpose()
    }

    async fn bind(&mut self, p: &Placement) -> StoreResult<()> {
        let explanation = to_json("explanation", &ExplanationDoc::from_domain(&p.explanation))?;
        sqlx::query_file!(
            "queries/placement_bind.sql",
            p.workload,
            p.node,
            p.intent,
            bounds_doc(&p.reserved),
            from_time(p.bound_at),
            p.bound_by.as_str(),
            p.reason,
            explanation,
            p.authoritative
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn unbind(&mut self, workload: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM placement WHERE workload_id = $1", workload)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        self.clear_unplaced(workload).await
    }

    async fn record_unplaced(&mut self, u: &Unplaced) -> StoreResult<bool> {
        let explanation = to_json("explanation", &ExplanationDoc::from_domain(&u.explanation))?;
        let previous = sqlx::query_file_scalar!(
            "queries/unplaced_record.sql",
            u.workload,
            u.intent,
            from_time(u.since),
            explanation
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        // No row: the same intent and explanation were already recorded, and nothing changed.
        Ok(previous.is_some_and(|before| before != Some(u.intent)))
    }

    async fn clear_unplaced(&mut self, workload: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM placement_failure WHERE workload_id = $1", workload)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn refusals(
        &mut self,
        workload: Uuid,
        intent: Uuid,
    ) -> StoreResult<BTreeMap<Uuid, String>> {
        let rows = sqlx::query!(
            "SELECT node_id, reason FROM placement_refusal
              WHERE workload_id = $1 AND intent_id = $2",
            workload,
            intent
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.node_id, r.reason)).collect())
    }

    async fn refuse(
        &mut self,
        (workload, intent): (Uuid, Uuid),
        node: Uuid,
        reason: &str,
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO placement_refusal (workload_id, intent_id, node_id, reason, refused_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (workload_id, intent_id, node_id)
             DO UPDATE SET reason = EXCLUDED.reason, refused_at = EXCLUDED.refused_at",
            workload,
            intent,
            node,
            reason,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
