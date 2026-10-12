// SPDX-License-Identifier: AGPL-3.0-or-later

//! The effect log: append-only, partitioned on the ingest clock (§4 of docs/03-state.md).

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, principal, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{
    EffectCursor, EffectFilter, EffectRead, EffectWrite, StoreError, StoreResult,
};
use korpis_core::audit::{Effect, JournalOrigin, Outcome};
use serde_json::{Map, Value};
use std::time::SystemTime;
use uuid::Uuid;

struct EffectRow {
    id: Uuid,
    at: DateTime<Utc>,
    occurred_at: Option<DateTime<Utc>>,
    plan_id: Option<Uuid>,
    step: Option<i32>,
    workload_id: Option<Uuid>,
    node_id: Option<Uuid>,
    actor: String,
    grant_id: Option<Uuid>,
    action: String,
    outcome: String,
    error: Option<Value>,
    before: Option<Value>,
    after: Option<Value>,
    count: i64,
    origin_node: Option<Uuid>,
    origin_seq: Option<i64>,
}

fn object(what: &str, v: Option<Value>) -> StoreResult<Option<Map<String, Value>>> {
    v.map(|v| from_json(what, v)).transpose()
}

impl EffectRow {
    fn into_domain(self) -> StoreResult<Effect> {
        let outcome = Outcome::parse(&self.outcome)
            .ok_or_else(|| StoreError::Corrupt(format!("effect outcome {:?}", self.outcome)))?;
        let origin = match (self.origin_node, self.origin_seq) {
            (Some(node), Some(seq)) => Some(JournalOrigin { node, seq: count("origin_seq", seq)? }),
            _ => None,
        };
        Ok(Effect {
            id: self.id,
            at: Some(to_time(self.at)),
            occurred_at: self.occurred_at.map(to_time),
            plan: self.plan_id,
            step: self
                .step
                .map(|s| u32::try_from(s).map_err(|_| StoreError::Corrupt("step".into())))
                .transpose()?,
            workload: self.workload_id,
            node: self.node_id,
            actor: principal(&self.actor)?,
            grant: self.grant_id,
            action: self.action,
            outcome,
            error: object("error", self.error)?,
            before: object("before", self.before)?,
            after: object("after", self.after)?,
            count: count("count", self.count)?,
            origin,
        })
    }
}

fn json(what: &str, m: Option<&Map<String, Value>>) -> StoreResult<Option<Value>> {
    m.map(|m| to_json(what, m)).transpose()
}

impl EffectRead for PgStore {
    async fn effects(
        &self,
        f: &EffectFilter,
        after: Option<EffectCursor>,
        limit: u32,
    ) -> StoreResult<Vec<Effect>> {
        let rows = sqlx::query_as!(
            EffectRow,
            "SELECT id, at, occurred_at, plan_id, step, workload_id, node_id, actor, grant_id,
                    action, outcome, error, before, after, count, origin_node, origin_seq
               FROM effect
              WHERE ($1::uuid IS NULL OR workload_id = $1)
                AND ($2::uuid IS NULL OR plan_id = $2)
                AND ($3::uuid IS NULL OR node_id = $3)
                AND ($4::timestamptz IS NULL OR at >= $4)
                AND ($5::timestamptz IS NULL OR (at, id) > ($5, $6))
              ORDER BY at, id LIMIT $7",
            f.workload,
            f.plan,
            f.node,
            f.since.map(from_time),
            after.map(|c| from_time(c.at)),
            after.map(|c| c.id),
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(EffectRow::into_domain).collect()
    }
}

impl EffectWrite for PgTx {
    async fn append_effect(&mut self, e: &Effect) -> StoreResult<SystemTime> {
        let step = e
            .step
            .map(|s| i32::try_from(s).map_err(|_| StoreError::Corrupt("step".into())))
            .transpose()?;
        let (origin_node, origin_seq) = match e.origin {
            Some(o) => (Some(o.node), Some(to_count("origin_seq", o.seq)?)),
            None => (None, None),
        };
        let at = sqlx::query_scalar!(
            "INSERT INTO effect (id, occurred_at, plan_id, step, workload_id, node_id, actor,
                                 grant_id, action, outcome, error, before, after, count,
                                 origin_node, origin_seq)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
             RETURNING at",
            e.id,
            e.occurred_at.map(from_time),
            e.plan,
            step,
            e.workload,
            e.node,
            e.actor.key(),
            e.grant,
            e.action,
            e.outcome.as_str(),
            json("error", e.error.as_ref())?,
            json("before", e.before.as_ref())?,
            json("after", e.after.as_ref())?,
            to_count("count", e.count)?,
            origin_node,
            origin_seq
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(to_time(at))
    }
}
