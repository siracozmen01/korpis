// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event subscriptions (§5 of docs/15-observability.md, migration 0025).

use crate::errors::from_sqlx;
use crate::stored::{
    SelectorDoc, count, from_json, from_time, principal, to_count, to_json, to_time,
};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{StoreError, StoreResult, SubscriptionRead, SubscriptionWrite};
use korpis_core::event::{Event, EventFilter, Severity, Subscription};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The store's own form of a filter.
#[derive(Debug, Serialize, Deserialize)]
struct FilterDoc {
    #[serde(default)]
    kinds: Vec<String>,
    min_severity: String,
    #[serde(default)]
    projects: Vec<Uuid>,
    workloads: SelectorDoc,
}

impl FilterDoc {
    fn from_domain(f: &EventFilter) -> Self {
        Self {
            kinds: f.kinds.clone(),
            min_severity: f.min_severity.as_str().to_owned(),
            projects: f.projects.clone(),
            workloads: SelectorDoc::from_domain(&f.workloads),
        }
    }

    fn into_domain(self) -> StoreResult<EventFilter> {
        let min_severity = Severity::parse(&self.min_severity)
            .ok_or_else(|| StoreError::Corrupt(format!("severity {:?}", self.min_severity)))?;
        Ok(EventFilter {
            kinds: self.kinds,
            min_severity,
            projects: self.projects,
            workloads: self.workloads.into_domain(),
        })
    }
}

struct EventRow {
    position: i64,
    key: String,
    seq: i64,
    at: DateTime<Utc>,
    kind: String,
    severity: String,
    organization_id: Uuid,
    workload_id: Option<Uuid>,
    project_id: Option<Uuid>,
    labels: Option<serde_json::Value>,
    effect_id: Option<Uuid>,
    detail: serde_json::Value,
}

impl EventRow {
    fn into_domain(self) -> StoreResult<Event> {
        let severity = Severity::parse(&self.severity)
            .ok_or_else(|| StoreError::Corrupt(format!("severity {:?}", self.severity)))?;
        let labels = match self.labels {
            Some(l) => from_json("workload labels", l)?,
            None => std::collections::BTreeMap::new(),
        };
        let serde_json::Value::Object(detail) = self.detail else {
            return Err(StoreError::Corrupt("event detail is not an object".into()));
        };
        Ok(Event {
            position: count("position", self.position)?,
            key: self.key,
            seq: count("seq", self.seq)?,
            at: to_time(self.at),
            kind: self.kind,
            severity,
            organization: self.organization_id,
            workload: self.workload_id,
            project: self.project_id,
            labels,
            effect: self.effect_id,
            detail,
        })
    }
}

struct SubscriptionRow {
    id: Uuid,
    subject_key: String,
    filter: serde_json::Value,
    acked: i64,
    created_at: DateTime<Utc>,
    extension_id: Option<Uuid>,
}

impl SubscriptionRow {
    fn into_domain(self) -> StoreResult<Subscription> {
        let filter: FilterDoc = from_json("subscription filter", self.filter)?;
        Ok(Subscription {
            id: self.id,
            subject: principal(&self.subject_key)?,
            filter: filter.into_domain()?,
            acked: count("acked", self.acked)?,
            created: to_time(self.created_at),
            extension: self.extension_id,
        })
    }
}

impl SubscriptionRead for PgStore {
    async fn subscription(&self, id: Uuid) -> StoreResult<Option<Subscription>> {
        let row = sqlx::query_as!(
            SubscriptionRow,
            "SELECT id, subject_key, filter, acked, created_at, extension_id
               FROM event_subscription WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(SubscriptionRow::into_domain).transpose()
    }

    async fn last_position(&self) -> StoreResult<u64> {
        let next = sqlx::query_scalar!("SELECT next FROM event_sequencer")
            .fetch_one(&self.pool)
            .await
            .map_err(from_sqlx)?;
        count("last position", next - 1)
    }

    async fn first_position(&self) -> StoreResult<Option<u64>> {
        let first = sqlx::query_scalar!("SELECT min(position) FROM event")
            .fetch_one(&self.pool)
            .await
            .map_err(from_sqlx)?;
        first.map(|p| count("first position", p)).transpose()
    }

    async fn events_after(&self, after: u64, limit: u32) -> StoreResult<Vec<Event>> {
        let rows = sqlx::query_as!(
            EventRow,
            r#"SELECT e.position AS "position!", e.key, e.seq, e.at, e.kind, e.severity,
                      e.organization_id, e.workload_id, w.project_id AS "project_id?",
                      w.labels AS "labels?", e.effect_id, e.detail
                 FROM event e LEFT JOIN workload w ON w.id = e.workload_id
                WHERE e.position > $1 ORDER BY e.position LIMIT $2"#,
            to_count("after", after)?,
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(EventRow::into_domain).collect()
    }

    async fn subscriptions_of(&self, subject: &str, page: Page) -> StoreResult<Vec<Subscription>> {
        let rows = sqlx::query_as!(
            SubscriptionRow,
            "SELECT id, subject_key, filter, acked, created_at, extension_id
               FROM event_subscription
              WHERE subject_key = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            subject,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(SubscriptionRow::into_domain).collect()
    }
}

impl SubscriptionWrite for PgTx {
    async fn insert_subscription(&mut self, s: &Subscription) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO event_subscription (id, subject_key, filter, acked, created_at,
                                             extension_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
            s.id,
            s.subject.key(),
            to_json("subscription filter", &FilterDoc::from_domain(&s.filter))?,
            to_count("acked", s.acked)?,
            from_time(s.created),
            s.extension
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_subscription(&mut self, id: Uuid) -> StoreResult<bool> {
        let n = sqlx::query!("DELETE FROM event_subscription WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?
            .rows_affected();
        Ok(n == 1)
    }

    async fn delete_extension_subscriptions(&mut self, extension: Uuid) -> StoreResult<u64> {
        let done =
            sqlx::query!("DELETE FROM event_subscription WHERE extension_id = $1", extension)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn ack_subscription(&mut self, id: Uuid, through: u64) -> StoreResult<Option<u64>> {
        let acked = sqlx::query_scalar!(
            "UPDATE event_subscription SET acked = GREATEST(acked, $2) WHERE id = $1
             RETURNING acked",
            id,
            to_count("through", through)?
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        acked.map(|a| count("acked", a)).transpose()
    }
}
