// SPDX-License-Identifier: AGPL-3.0-or-later

//! The invocations of extensions' actions while they run (migration 0150).

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{InvocationRead, InvocationWrite, StoreError, StoreResult};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::action::Invocation;
use serde_json::{Value, json};
use std::time::SystemTime;
use uuid::Uuid;

fn resources_to(r: &[ObjectRef]) -> Value {
    Value::Array(r.iter().map(|o| json!({ "kind": o.kind.as_str(), "id": o.id })).collect())
}

fn resources_from(v: &Value) -> StoreResult<Vec<ObjectRef>> {
    let corrupt = || StoreError::Corrupt(format!("invocation resources: {v}"));
    let one = |o: &Value| {
        let kind = Kind::parse(o.get("kind")?.as_str()?)?;
        let id = o.get("id")?.as_str()?.parse().ok()?;
        Some(ObjectRef::new(kind, id))
    };
    v.as_array().ok_or_else(corrupt)?.iter().map(|o| one(o).ok_or_else(corrupt)).collect()
}

struct InvocationRow {
    id: Uuid,
    extension_id: Uuid,
    action: String,
    invoker: String,
    resources: Value,
    expires_at: DateTime<Utc>,
}

impl TryFrom<InvocationRow> for Invocation {
    type Error = StoreError;

    fn try_from(r: InvocationRow) -> StoreResult<Self> {
        Ok(Self {
            id: r.id,
            extension: r.extension_id,
            action: r.action,
            invoker: r.invoker,
            resources: resources_from(&r.resources)?,
            expires_at: to_time(r.expires_at),
        })
    }
}

impl InvocationRead for PgStore {
    async fn live_invocation(
        &self,
        digest: &[u8; 32],
        now: SystemTime,
    ) -> StoreResult<Option<Invocation>> {
        let row = sqlx::query_as!(
            InvocationRow,
            "SELECT id, extension_id, action, invoker, resources, expires_at
               FROM extension_invocation
              WHERE secret_digest = $1 AND finished_at IS NULL AND expires_at > $2",
            &digest[..],
            from_time(now)
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(Invocation::try_from).transpose()
    }
}

impl InvocationWrite for PgTx {
    async fn insert_invocation(
        &mut self,
        i: &Invocation,
        digest: &[u8; 32],
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO extension_invocation (id, extension_id, action, invoker, resources,
                                               secret_digest, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            i.id,
            i.extension,
            i.action,
            i.invoker,
            resources_to(&i.resources),
            &digest[..],
            from_time(at),
            from_time(i.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn finish_invocation(&mut self, id: Uuid, at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE extension_invocation SET finished_at = $2 WHERE id = $1 AND finished_at IS NULL",
            id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn forget_invocations(&mut self, before: SystemTime) -> StoreResult<u64> {
        let done = sqlx::query!(
            "DELETE FROM extension_invocation
              WHERE coalesce(finished_at, expires_at) < $1 AND expires_at < $1",
            from_time(before)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
