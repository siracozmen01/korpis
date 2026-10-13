// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider call queue (§3.3 of docs/16-extensions.md, migration 0027).

mod json;
mod value;

use crate::errors::from_sqlx;
use crate::provider::{RegistrationRow, kind};
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{ProviderCallRead, ProviderCallWrite, StoreResult};
use korpis_core::extension::{
    CallOutcome, ProvidedInterface, ProviderCall, ProviderType, Registration,
};
use std::time::SystemTime;
use uuid::Uuid;

struct CallRow {
    id: Uuid,
    organization_id: Uuid,
    kind: String,
    name: String,
    request: serde_json::Value,
    deadline: DateTime<Utc>,
    created_at: DateTime<Utc>,
    registration_id: Option<Uuid>,
    version: Option<String>,
    outcome: Option<serde_json::Value>,
    finished_at: Option<DateTime<Utc>>,
}

impl CallRow {
    fn into_domain(self) -> StoreResult<ProviderCall> {
        let finished = match (self.outcome, self.finished_at) {
            (Some(o), Some(at)) => Some((json::outcome_from(&o)?, to_time(at))),
            _ => None,
        };
        Ok(ProviderCall {
            id: self.id,
            organization: self.organization_id,
            kind: kind(&self.kind)?,
            name: self.name,
            request: json::request_from(&self.request)?,
            deadline: to_time(self.deadline),
            created_at: to_time(self.created_at),
            sent: self.registration_id.zip(self.version),
            finished,
        })
    }
}

impl ProviderCallRead for PgStore {
    async fn providing_organizations(
        &self,
        organizations: &[Uuid],
        k: ProviderType,
        name: &str,
    ) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT organization_id FROM extension_provides
              WHERE organization_id = ANY($1) AND type = $2 AND name = $3",
            organizations,
            k.as_str(),
            name
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn provides_in(&self, organization: Uuid) -> StoreResult<Vec<ProvidedInterface>> {
        let rows = sqlx::query!(
            "SELECT type, name FROM extension_provides WHERE organization_id = $1
              ORDER BY type, name",
            organization
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| Ok(ProvidedInterface { kind: kind(&r.r#type)?, name: r.name }))
            .collect()
    }

    async fn registrations_in(&self, organization: Uuid) -> StoreResult<Vec<Registration>> {
        let rows = sqlx::query_as!(
            RegistrationRow,
            r#"SELECT id, extension_id, organization_id, type AS kind, name, version,
                      concurrency, replica, connected_at, seen_at
                 FROM provider_registration
                WHERE organization_id = $1 ORDER BY type, name, connected_at, id"#,
            organization
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RegistrationRow::into_domain).collect()
    }

    async fn provider_call(&self, id: Uuid) -> StoreResult<Option<ProviderCall>> {
        let row = sqlx::query_as!(
            CallRow,
            r#"SELECT id, organization_id, type AS kind, name, request, deadline, created_at,
                      registration_id, version, outcome, finished_at
                 FROM provider_call WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(CallRow::into_domain).transpose()
    }
}

impl ProviderCallWrite for PgTx {
    async fn insert_call(&mut self, c: &ProviderCall) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO provider_call (id, organization_id, type, name, request, deadline,
                                        created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            c.id,
            c.organization,
            c.kind.as_str(),
            c.name,
            json::request_to(&c.request),
            from_time(c.deadline),
            from_time(c.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn claim_calls(
        &mut self,
        r: &Registration,
        at: SystemTime,
    ) -> StoreResult<Vec<ProviderCall>> {
        // `me` is empty unless this registration exists and serves the version registered most
        // recently for what it serves; `free` is then its unused concurrency.
        let rows = sqlx::query_as!(
            CallRow,
            r#"WITH me AS (
                   SELECT r.id, r.version, r.concurrency FROM provider_registration r
                    WHERE r.id = $1
                      AND r.version = (SELECT n.version FROM provider_registration n
                                        WHERE n.organization_id = r.organization_id
                                          AND n.type = r.type AND n.name = r.name
                                        ORDER BY n.connected_at DESC, n.id DESC LIMIT 1)
               ), free AS (
                   SELECT GREATEST(me.concurrency - (SELECT count(*) FROM provider_call c
                                                      WHERE c.registration_id = me.id
                                                        AND c.finished_at IS NULL), 0) AS n
                     FROM me
               ), picked AS (
                   SELECT c.id FROM provider_call c
                    WHERE c.organization_id = $2 AND c.type = $3 AND c.name = $4
                      AND c.registration_id IS NULL AND c.finished_at IS NULL
                      AND c.deadline > $5
                    ORDER BY c.created_at, c.id
                    LIMIT COALESCE((SELECT n FROM free), 0)
                    FOR UPDATE SKIP LOCKED
               )
               UPDATE provider_call c
                  SET registration_id = me.id, version = me.version, sent_at = $5
                 FROM picked, me
                WHERE c.id = picked.id
            RETURNING c.id, c.organization_id, c.type AS kind, c.name, c.request, c.deadline,
                      c.created_at, c.registration_id, c.version, c.outcome, c.finished_at"#,
            r.id,
            r.organization,
            r.kind.as_str(),
            r.name,
            from_time(at)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let mut calls: Vec<_> =
            rows.into_iter().map(CallRow::into_domain).collect::<Result<_, _>>()?;
        calls.sort_by_key(|c| (c.created_at, c.id));
        Ok(calls)
    }

    async fn finish_call(
        &mut self,
        id: Uuid,
        registration: Option<Uuid>,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE provider_call SET outcome = $3, finished_at = $4
              WHERE id = $1 AND finished_at IS NULL
                AND ($2::uuid IS NULL OR registration_id = $2)",
            id,
            registration,
            json::outcome_to(outcome),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn fail_calls_without_registration(
        &mut self,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE provider_call c SET outcome = $1, finished_at = $2
              WHERE c.registration_id IS NOT NULL AND c.finished_at IS NULL
                AND NOT EXISTS (SELECT 1 FROM provider_registration r
                                 WHERE r.id = c.registration_id)",
            json::outcome_to(outcome),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn fail_calls_without_provider(
        &mut self,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE provider_call c SET outcome = $1, finished_at = $2
              WHERE c.registration_id IS NULL AND c.finished_at IS NULL
                AND NOT EXISTS (SELECT 1 FROM extension_provides p
                                 WHERE p.organization_id = c.organization_id
                                   AND p.type = c.type AND p.name = c.name)",
            json::outcome_to(outcome),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn prune_calls(&mut self, before: SystemTime) -> StoreResult<u64> {
        let done =
            sqlx::query!("DELETE FROM provider_call WHERE finished_at < $1", from_time(before))
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
