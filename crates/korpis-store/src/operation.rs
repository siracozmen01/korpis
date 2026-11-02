// SPDX-License-Identifier: AGPL-3.0-or-later

//! Operations and their items (migration 0149).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::{PgStore, PgTx};
use korpis_app::Page;
use korpis_app::ports::{OperationAt, OperationRead, OperationWrite, StoreError, StoreResult};
use korpis_core::operation::{
    Approved, Item, ItemState, Kind, OnFailure, Operation, Policy, Scope, Status,
};
use sqlx::PgConnection;
use uuid::Uuid;

type Time = chrono::DateTime<chrono::Utc>;

fn corrupt(what: &str, v: &str) -> StoreError {
    StoreError::Corrupt(format!("operation {what} {v:?}"))
}

fn small(what: &str, n: i32) -> StoreResult<u32> {
    u32::try_from(n).map_err(|_| corrupt(what, &n.to_string()))
}

/// One stored operation.
struct OperationRow {
    id: Uuid,
    kind: String,
    node_id: Option<Uuid>,
    template_id: Option<Uuid>,
    template_version: Option<i64>,
    concurrency: i32,
    max_unavailable: i32,
    on_failure: String,
    status: String,
    reason: String,
    proposed_by: String,
    proposed_under: Uuid,
    proposed_at: Time,
    approved_by: Option<String>,
    approved_under: Option<Uuid>,
    approved_mfa: Option<bool>,
    approved_at: Option<Time>,
    start_after: Option<Time>,
    ended_at: Option<Time>,
    version: i64,
}

impl OperationRow {
    fn scope(&self) -> StoreResult<Scope> {
        match (self.node_id, self.template_id, self.template_version) {
            (Some(node), None, None) => Ok(Scope::Node(node)),
            (None, Some(template), Some(v)) => {
                let version = u64::try_from(v).map_err(|_| corrupt("version", &v.to_string()))?;
                Ok(Scope::Template { template, version })
            }
            _ => Err(corrupt("scope", &self.id.to_string())),
        }
    }

    fn approved(&self) -> StoreResult<Option<Approved>> {
        let (Some(by), Some(under), Some(mfa), Some(at)) =
            (&self.approved_by, self.approved_under, self.approved_mfa, self.approved_at)
        else {
            return Ok(None);
        };
        Ok(Some(Approved {
            by: principal(by)?,
            under,
            mfa,
            at: to_time(at),
            start_after: self.start_after.map(to_time),
        }))
    }

    fn into_domain(self) -> StoreResult<Operation> {
        let kind = Kind::parse(&self.kind).ok_or_else(|| corrupt("kind", &self.kind))?;
        let on_failure = OnFailure::parse(&self.on_failure)
            .ok_or_else(|| corrupt("policy", &self.on_failure))?;
        let status = Status::parse(&self.status).ok_or_else(|| corrupt("status", &self.status))?;
        let policy = Policy {
            concurrency: small("concurrency", self.concurrency)?,
            max_unavailable: small("max_unavailable", self.max_unavailable)?,
            on_failure,
        };
        Ok(Operation {
            id: self.id,
            kind,
            scope: self.scope()?,
            policy,
            status,
            approved: self.approved()?,
            reason: self.reason,
            proposed_by: principal(&self.proposed_by)?,
            proposed_under: self.proposed_under,
            proposed_at: to_time(self.proposed_at),
            ended_at: self.ended_at.map(to_time),
            version: u64::try_from(self.version).map_err(|_| corrupt("version", "negative"))?,
        })
    }
}

/// One stored item.
struct ItemRow {
    operation_id: Uuid,
    subject: Uuid,
    position: i32,
    state: String,
    plan_id: Option<Uuid>,
    issued: Option<Uuid>,
    detail: serde_json::Value,
    reason: String,
    begun_at: Option<Time>,
    ended_at: Option<Time>,
}

impl ItemRow {
    fn into_domain(self) -> StoreResult<Item> {
        let state = ItemState::parse(&self.state).ok_or_else(|| corrupt("item", &self.state))?;
        let serde_json::Value::Object(detail) = self.detail else {
            return Err(corrupt("item detail", &self.subject.to_string()));
        };
        Ok(Item {
            operation: self.operation_id,
            subject: self.subject,
            position: small("position", self.position)?,
            state,
            plan: self.plan_id,
            issued: self.issued,
            detail,
            reason: self.reason,
            begun_at: self.begun_at.map(to_time),
            ended_at: self.ended_at.map(to_time),
        })
    }
}

/// The items of `operation`, in order.
async fn items(conn: &mut PgConnection, operation: Uuid) -> StoreResult<Vec<Item>> {
    sqlx::query_as!(
        ItemRow,
        "SELECT operation_id, subject, position, state, plan_id, issued, detail, reason, begun_at,
                ended_at
           FROM operation_item WHERE operation_id = $1 ORDER BY position",
        operation
    )
    .fetch_all(conn)
    .await
    .map_err(from_sqlx)?
    .into_iter()
    .map(ItemRow::into_domain)
    .collect()
}

/// The operation `id`, locked if `lock`, with its items.
async fn read(conn: &mut PgConnection, id: Uuid, lock: bool) -> StoreResult<Option<OperationAt>> {
    let row = if lock {
        sqlx::query_as!(
            OperationRow,
            "SELECT id, kind, node_id, template_id, template_version, concurrency,
                    max_unavailable, on_failure, status, reason, proposed_by, proposed_under,
                    proposed_at, approved_by, approved_under, approved_mfa, approved_at,
                    start_after, ended_at, version
               FROM operation WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut *conn)
        .await
    } else {
        sqlx::query_as!(
            OperationRow,
            "SELECT id, kind, node_id, template_id, template_version, concurrency,
                    max_unavailable, on_failure, status, reason, proposed_by, proposed_under,
                    proposed_at, approved_by, approved_under, approved_mfa, approved_at,
                    start_after, ended_at, version
               FROM operation WHERE id = $1",
            id
        )
        .fetch_optional(&mut *conn)
        .await
    }
    .map_err(from_sqlx)?;
    let Some(row) = row else { return Ok(None) };
    let op = row.into_domain()?;
    Ok(Some((op, items(conn, id).await?)))
}

impl OperationRead for PgStore {
    async fn operation(&self, id: Uuid) -> StoreResult<Option<OperationAt>> {
        let mut conn = self.pool.acquire().await.map_err(from_sqlx)?;
        read(&mut conn, id, false).await
    }

    async fn operations(&self, node: Option<Uuid>, page: Page) -> StoreResult<Vec<Operation>> {
        sqlx::query_as!(
            OperationRow,
            "SELECT id, kind, node_id, template_id, template_version, concurrency,
                    max_unavailable, on_failure, status, reason, proposed_by, proposed_under,
                    proposed_at, approved_by, approved_under, approved_mfa, approved_at,
                    start_after, ended_at, version
               FROM operation
              WHERE ($1::uuid IS NULL OR id < $1) AND ($2::uuid IS NULL OR node_id = $2)
              ORDER BY id DESC LIMIT $3",
            page.after,
            node,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?
        .into_iter()
        .map(OperationRow::into_domain)
        .collect()
    }

    async fn live_operations(&self) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT id FROM operation WHERE status IN ('approved', 'running', 'paused')
              ORDER BY proposed_at, id"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }
}

impl OperationWrite for PgTx {
    async fn insert_operation(&mut self, op: &Operation, items: &[Item]) -> StoreResult<()> {
        let (node, template, version) = match op.scope {
            Scope::Node(n) => (Some(n), None, None),
            Scope::Template { template, version } => {
                (None, Some(template), Some(i64::try_from(version).unwrap_or(i64::MAX)))
            }
        };
        sqlx::query!(
            "INSERT INTO operation (id, kind, node_id, template_id, template_version, concurrency,
                                    max_unavailable, on_failure, status, reason, proposed_by,
                                    proposed_under, proposed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            op.id,
            op.kind.as_str(),
            node,
            template,
            version,
            i32::try_from(op.policy.concurrency).unwrap_or(i32::MAX),
            i32::try_from(op.policy.max_unavailable).unwrap_or(i32::MAX),
            op.policy.on_failure.as_str(),
            op.status.as_str(),
            op.reason,
            op.proposed_by.key(),
            op.proposed_under,
            from_time(op.proposed_at),
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for item in items {
            self.put_item(item).await?;
        }
        Ok(())
    }

    async fn operation_in(&mut self, id: Uuid) -> StoreResult<Option<OperationAt>> {
        read(&mut self.tx, id, true).await
    }

    async fn update_operation(&mut self, op: &Operation, read_at: u64) -> StoreResult<bool> {
        let a = op.approved.as_ref();
        let done = sqlx::query!(
            "UPDATE operation
                SET status = $3, reason = $4, approved_by = $5, approved_under = $6,
                    approved_mfa = $7, approved_at = $8, start_after = $9, ended_at = $10,
                    version = version + 1
              WHERE id = $1 AND version = $2",
            op.id,
            i64::try_from(read_at).unwrap_or(i64::MAX),
            op.status.as_str(),
            op.reason,
            a.map(|a| a.by.key()),
            a.map(|a| a.under),
            a.map(|a| a.mfa),
            a.map(|a| from_time(a.at)),
            a.and_then(|a| a.start_after).map(from_time),
            op.ended_at.map(from_time),
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn put_item(&mut self, item: &Item) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO operation_item (operation_id, subject, position, state, plan_id, issued,
                                         detail, reason, begun_at, ended_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (operation_id, subject) DO UPDATE
                SET state = $4, plan_id = $5, issued = $6, detail = $7, reason = $8,
                    begun_at = $9, ended_at = $10",
            item.operation,
            item.subject,
            i32::try_from(item.position).unwrap_or(i32::MAX),
            item.state.as_str(),
            item.plan,
            item.issued,
            serde_json::Value::Object(item.detail.clone()),
            item.reason,
            item.begun_at.map(from_time),
            item.ended_at.map(from_time),
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
