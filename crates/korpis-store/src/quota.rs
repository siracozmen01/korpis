// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota entries and what the workloads under them hold (§5.1 of docs/05-scheduling.md).

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{count, from_json};
use crate::tenancy::labels;
use korpis_app::ports::{Holder, QuotaWrite, StoreError, StoreResult};
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::quota::{Bounds, Dimension, Held, QuotaEntry, QuotaOwner};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// A selector, as an entry stores it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SelectorDoc {
    #[serde(default)]
    match_labels: BTreeMap<String, String>,
    #[serde(default)]
    expressions: Vec<crate::stored::ExpressionDoc>,
}

fn bounds(what: &str, v: Value) -> StoreResult<Bounds> {
    let raw: BTreeMap<String, u64> = from_json(what, v)?;
    raw.into_iter()
        .map(|(k, n)| {
            Dimension::parse(&k)
                .map(|d| (d, n))
                .ok_or_else(|| StoreError::Corrupt(format!("{what}: dimension {k:?}")))
        })
        .collect()
}

struct EntryRow {
    id: Uuid,
    owner_kind: String,
    owner_id: Uuid,
    selector: Option<Value>,
    reservations: Value,
    limits: Value,
    version: i64,
}

impl EntryRow {
    fn into_domain(self) -> StoreResult<QuotaEntry> {
        let owner = match self.owner_kind.as_str() {
            "organization" => QuotaOwner::Organization(self.owner_id),
            "project" => QuotaOwner::Project(self.owner_id),
            other => return Err(StoreError::Corrupt(format!("quota owner {other:?}"))),
        };
        let selector = self
            .selector
            .map(|s| {
                let doc: SelectorDoc = from_json("quota selector", s)?;
                Ok::<_, StoreError>(LabelSelector {
                    match_labels: doc.match_labels,
                    expressions: doc
                        .expressions
                        .into_iter()
                        .map(crate::stored::expression)
                        .collect(),
                })
            })
            .transpose()?;
        Ok(QuotaEntry {
            id: self.id,
            owner,
            selector,
            reservations: bounds("quota reservations", self.reservations)?,
            limits: bounds("quota limits", self.limits)?,
            version: count("quota version", self.version)?,
        })
    }
}

impl QuotaWrite for PgTx {
    async fn lock_quota(
        &mut self,
        project: Uuid,
        organizations: &[Uuid],
    ) -> StoreResult<Vec<QuotaEntry>> {
        let rows = sqlx::query_file_as!(EntryRow, "queries/quota_lock.sql", project, organizations)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter().map(EntryRow::into_domain).collect()
    }

    async fn holders(&mut self, owner: QuotaOwner, except: Uuid) -> StoreResult<Vec<Holder>> {
        let (project, organization) = match owner {
            QuotaOwner::Project(p) => (Some(p), None),
            QuotaOwner::Organization(o) => (None, Some(o)),
        };
        let rows = sqlx::query_file!("queries/quota_holders.sql", project, organization, except)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let decode = |json: &str, version: &str| {
            self.bodies
                .decode(json, version)
                .map_err(|e| StoreError::Corrupt(format!("intent body: {e}")))
        };
        rows.into_iter()
            .map(|r| {
                let mut held = Held::of(&decode(&r.head, &r.head_schema)?);
                if let (Some(t), Some(v)) = (r.target, r.target_schema) {
                    held = held.max(&Held::of(&decode(&t, &v)?));
                }
                let labels = labels(r.labels)?.as_map().clone();
                Ok(Holder { workload: r.id, labels, held })
            })
            .collect()
    }

    async fn consume_quota(&mut self, entries: &[Uuid]) -> StoreResult<()> {
        sqlx::query!("UPDATE quota_entry SET version = version + 1 WHERE id = ANY($1)", entries)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }
}
