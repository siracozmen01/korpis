// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota entries and what the workloads under them hold (§5.1 of docs/05-scheduling.md), with
//! the totals kept beside each entry (question 7 of §10 there).

use crate::errors::from_sqlx;
use crate::stored::{
    SelectorDoc, bounds, count, from_json, held, held_doc, to_json, totals, totals_doc,
};
use crate::tenancy::labels;
use crate::{PgStore, PgTx};
use korpis_app::ports::{Counted, Holding, QuotaRead, QuotaWrite, StoreError, StoreResult};
use korpis_core::tenancy::quota::{
    EgressBudget, Exhaustion, Held, QuotaEntry, QuotaOwner, QuotaSet, Totals,
};
use serde_json::Value;
use uuid::Uuid;

mod placed;
mod set;

/// A quota entry as stored.
pub(crate) struct EntryRow {
    pub(crate) id: Uuid,
    pub(crate) owner_kind: String,
    pub(crate) owner_id: Uuid,
    pub(crate) selector: Option<Value>,
    pub(crate) nodes: Option<Value>,
    pub(crate) reservations: Value,
    pub(crate) limits: Value,
    pub(crate) version: i64,
    pub(crate) held: Option<Value>,
    pub(crate) egress_budget: Option<Value>,
}

/// An entry of an organization beneath another, with the one it is beneath.
struct PromisedRow {
    id: Uuid,
    owner_kind: String,
    owner_id: Uuid,
    selector: Option<Value>,
    nodes: Option<Value>,
    reservations: Value,
    limits: Value,
    version: i64,
    held: Option<Value>,
    egress_budget: Option<Value>,
    parent: Uuid,
}

impl PromisedRow {
    fn into_domain(self) -> StoreResult<(Uuid, QuotaEntry)> {
        let entry = EntryRow {
            id: self.id,
            owner_kind: self.owner_kind,
            owner_id: self.owner_id,
            selector: self.selector,
            nodes: self.nodes,
            reservations: self.reservations,
            limits: self.limits,
            version: self.version,
            held: self.held,
            egress_budget: self.egress_budget,
        };
        Ok((self.parent, entry.into_domain()?))
    }
}

/// An entry's egress budget as stored: the bytes a month, and a floor or a cut-off.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BudgetDoc {
    bytes_per_month: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    floor_bits_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    cut_off: bool,
}

/// A budget, to store.
pub(crate) fn budget_doc(b: Option<&EgressBudget>) -> StoreResult<Option<Value>> {
    b.map(|b| {
        let (floor_bits_per_sec, cut_off) = match b.exhausted {
            Exhaustion::Floor(bits) => (Some(bits), false),
            Exhaustion::CutOff => (None, true),
        };
        to_json(
            "egress budget",
            &BudgetDoc { bytes_per_month: b.bytes_per_month, floor_bits_per_sec, cut_off },
        )
    })
    .transpose()
}

fn budget(v: Option<Value>) -> StoreResult<Option<EgressBudget>> {
    v.map(|v| {
        let d: BudgetDoc = from_json("egress budget", v)?;
        let exhausted = match (d.cut_off, d.floor_bits_per_sec) {
            (true, _) => Exhaustion::CutOff,
            (false, Some(bits)) => Exhaustion::Floor(bits),
            (false, None) => {
                return Err(StoreError::Corrupt("an egress budget with no floor".into()));
            }
        };
        Ok(EgressBudget { bytes_per_month: d.bytes_per_month, exhausted, spent: None })
    })
    .transpose()
}

fn selector(
    what: &str,
    v: Option<Value>,
) -> StoreResult<Option<korpis_core::labels::LabelSelector>> {
    v.map(|s| {
        let doc: SelectorDoc = from_json(what, s)?;
        Ok(doc.into_domain())
    })
    .transpose()
}

impl EntryRow {
    pub(crate) fn into_domain(self) -> StoreResult<QuotaEntry> {
        let owner = match self.owner_kind.as_str() {
            "organization" => QuotaOwner::Organization(self.owner_id),
            "project" => QuotaOwner::Project(self.owner_id),
            other => return Err(StoreError::Corrupt(format!("quota owner {other:?}"))),
        };
        Ok(QuotaEntry {
            id: self.id,
            owner,
            selector: selector("quota selector", self.selector)?,
            nodes: selector("quota nodes", self.nodes)?,
            reservations: bounds("quota reservations", self.reservations)?,
            limits: bounds("quota limits", self.limits)?,
            version: count("quota version", self.version)?,
            egress: budget(self.egress_budget)?,
            held: self.held.map(totals).transpose()?,
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

    async fn consume_quota(&mut self, entries: &[Uuid]) -> StoreResult<()> {
        sqlx::query!("UPDATE quota_entry SET version = version + 1 WHERE id = ANY($1)", entries)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn promised_entries(
        &mut self,
        organizations: &[Uuid],
    ) -> StoreResult<Vec<(Uuid, QuotaEntry)>> {
        let rows = sqlx::query_file_as!(PromisedRow, "queries/quota_promised.sql", organizations)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter().map(PromisedRow::into_domain).collect()
    }

    async fn lock_quota_entry(&mut self, id: Uuid) -> StoreResult<Option<QuotaEntry>> {
        let row = sqlx::query_as!(
            EntryRow,
            "SELECT id, owner_kind, owner_id, selector, nodes, reservations, limits, version, held,
                    egress_budget
               FROM quota_entry WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(EntryRow::into_domain).transpose()
    }

    async fn put_quota_totals(&mut self, entry: Uuid, totals: &Totals) -> StoreResult<()> {
        sqlx::query!("UPDATE quota_entry SET held = $2 WHERE id = $1", entry, totals_doc(totals)?)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn counted(&mut self, workload: Uuid) -> StoreResult<Option<Counted>> {
        let row = sqlx::query!(
            "SELECT held, counted_by FROM workload_held WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| Ok(Counted { held: held(r.held)?, by: r.counted_by })).transpose()
    }

    async fn put_counted(&mut self, workload: Uuid, counted: &Counted) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO workload_held (workload_id, held, counted_by) VALUES ($1, $2, $3)
             ON CONFLICT (workload_id) DO UPDATE SET held = $2, counted_by = $3",
            workload,
            held_doc(&counted.held)?,
            &counted.by
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn holdings(&mut self, owner: QuotaOwner) -> StoreResult<Vec<Holding>> {
        let (project, organization) = match owner {
            QuotaOwner::Project(p) => (Some(p), None),
            QuotaOwner::Organization(o) => (None, Some(o)),
        };
        let rows = sqlx::query_file!("queries/quota_holdings.sql", project, organization)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(Holding {
                    workload: r.id,
                    labels: labels(r.labels)?.as_map().clone(),
                    counted: Counted { held: held(r.held)?, by: r.counted_by },
                })
            })
            .collect()
    }

    async fn count_in(&mut self, entry: Uuid, workloads: &[Uuid]) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE workload_held SET counted_by = array_append(counted_by, $1)
              WHERE workload_id = ANY($2) AND NOT ($1 = ANY(counted_by))",
            entry,
            workloads
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn counted_by(&mut self, entry: Uuid) -> StoreResult<Vec<Held>> {
        let rows = sqlx::query_scalar!(
            "SELECT held FROM workload_held WHERE $1 = ANY(counted_by) ORDER BY workload_id",
            entry
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(held).collect()
    }
}

impl QuotaRead for PgStore {
    async fn quota_entry_ids(&self) -> StoreResult<Vec<(Uuid, bool)>> {
        let rows = sqlx::query!(
            r#"SELECT id, nodes IS NOT NULL AS "placed!" FROM quota_entry ORDER BY id"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.id, r.placed)).collect())
    }

    async fn workloads_to_count(&self) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            r#"SELECT id AS "id!" FROM workload WHERE deleted_at IS NULL
               UNION
               SELECT workload_id FROM workload_held WHERE cardinality(counted_by) > 0
               UNION
               SELECT workload_id FROM placement_held
               ORDER BY 1"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn quota_set(&self, owner: QuotaOwner) -> StoreResult<QuotaSet> {
        self.read_quota_set(owner).await
    }
}
