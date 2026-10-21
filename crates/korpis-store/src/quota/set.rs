// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota sets as their owners' parents set them (§5.1 of docs/05-scheduling.md): the entries,
//! each replaced, kept, or removed by shape, and the overcommit an owner permits its children.

use super::EntryRow;
use crate::errors::from_sqlx;
use crate::stored::{SelectorDoc, bounds_doc, to_json};
use crate::{PgStore, PgTx};
use korpis_app::ports::{QuotaSetWrite, StoreError, StoreResult};
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::quota::{
    Bounds, Dimension, EgressBudget, QuotaEntry, QuotaOwner, QuotaSet,
};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The stored kind and identifier of `owner`.
fn owned(owner: QuotaOwner) -> (&'static str, Uuid) {
    match owner {
        QuotaOwner::Organization(o) => ("organization", o),
        QuotaOwner::Project(p) => ("project", p),
    }
}

fn selector_doc(s: Option<&LabelSelector>) -> StoreResult<Option<serde_json::Value>> {
    s.map(|s| to_json("quota selector", &SelectorDoc::from_domain(s))).transpose()
}

fn percents(rows: Vec<(String, i32)>) -> StoreResult<BTreeMap<Dimension, u32>> {
    rows.into_iter()
        .map(|(d, p)| {
            let dimension = Dimension::parse(&d)
                .ok_or_else(|| StoreError::Corrupt(format!("overcommit dimension {d:?}")))?;
            let percent = u32::try_from(p)
                .map_err(|_| StoreError::Corrupt(format!("overcommit percent {p}")))?;
            Ok((dimension, percent))
        })
        .collect()
}

impl PgStore {
    /// The quota set of `owner`, with what each entry's workloads hold.
    pub(crate) async fn read_quota_set(&self, owner: QuotaOwner) -> StoreResult<QuotaSet> {
        let (kind, id) = owned(owner);
        let rows = sqlx::query_as!(
            EntryRow,
            "SELECT id, owner_kind, owner_id, selector, nodes, reservations, limits, version, held,
                    egress_budget
               FROM quota_entry WHERE owner_kind = $1 AND owner_id = $2 ORDER BY id",
            kind,
            id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut entries: Vec<QuotaEntry> =
            rows.into_iter().map(EntryRow::into_domain).collect::<StoreResult<_>>()?;
        let ids: Vec<Uuid> = entries.iter().map(|e| e.id).collect();
        let spent = korpis_app::ports::EgressRead::quota_egress_spent(self, &ids).await?;
        for e in &mut entries {
            if let Some(b) = e.egress.as_mut() {
                b.spent = spent.get(&e.id).copied();
            }
        }
        let percent = sqlx::query_as::<_, (String, i32)>(
            "SELECT dimension, percent FROM quota_overcommit
              WHERE owner_kind = $1 AND owner_id = $2",
        )
        .bind(kind)
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(QuotaSet { entries, child_overcommit: percents(percent)? })
    }
}

impl QuotaSetWrite for PgTx {
    async fn lock_quota_owners(&mut self, organizations: &[Uuid]) -> StoreResult<()> {
        // Not FOR UPDATE: that would also wait out every insert of a project or a workload
        // referencing the organization, which takes a key-share lock on its row.
        for id in organizations {
            sqlx::query!("SELECT id FROM organization WHERE id = $1 FOR NO KEY UPDATE", id)
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn lock_quota_sets(
        &mut self,
        organizations: &[Uuid],
        projects: &[Uuid],
    ) -> StoreResult<Vec<QuotaEntry>> {
        let rows =
            sqlx::query_file_as!(EntryRow, "queries/quota_lock_sets.sql", organizations, projects)
                .fetch_all(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        rows.into_iter().map(EntryRow::into_domain).collect()
    }

    async fn overcommit(&mut self, owner: QuotaOwner) -> StoreResult<BTreeMap<Dimension, u32>> {
        let (kind, id) = owned(owner);
        let rows = sqlx::query_as::<_, (String, i32)>(
            "SELECT dimension, percent FROM quota_overcommit
              WHERE owner_kind = $1 AND owner_id = $2",
        )
        .bind(kind)
        .bind(id)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        percents(rows)
    }

    async fn put_overcommit(
        &mut self,
        owner: QuotaOwner,
        percent: &BTreeMap<Dimension, u32>,
    ) -> StoreResult<()> {
        let (kind, id) = owned(owner);
        sqlx::query!(
            "DELETE FROM quota_overcommit WHERE owner_kind = $1 AND owner_id = $2",
            kind,
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for (d, p) in percent {
            let p = i32::try_from(*p)
                .map_err(|_| StoreError::Failed(format!("overcommit percent {p} out of range")))?;
            sqlx::query!(
                "INSERT INTO quota_overcommit (owner_kind, owner_id, dimension, percent)
                 VALUES ($1, $2, $3, $4)",
                kind,
                id,
                d.as_str(),
                p
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn insert_quota_entry(&mut self, e: &QuotaEntry) -> StoreResult<()> {
        let (kind, owner) = owned(e.owner);
        sqlx::query!(
            "INSERT INTO quota_entry (id, owner_kind, owner_id, selector, nodes, reservations,
                                      limits, egress_budget)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            e.id,
            kind,
            owner,
            selector_doc(e.selector.as_ref())?,
            selector_doc(e.nodes.as_ref())?,
            bounds_doc(&e.reservations),
            bounds_doc(&e.limits),
            super::budget_doc(e.egress.as_ref())?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn update_quota_bounds(
        &mut self,
        id: Uuid,
        (reservations, limits): (&Bounds, &Bounds),
        egress: Option<&EgressBudget>,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE quota_entry SET reservations = $2, limits = $3, egress_budget = $4,
                    version = version + 1
              WHERE id = $1",
            id,
            bounds_doc(reservations),
            bounds_doc(limits),
            super::budget_doc(egress)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_quota_entry(&mut self, id: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM quota_entry WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn quota_children(&mut self, organization: Uuid) -> StoreResult<(Vec<Uuid>, Vec<Uuid>)> {
        let orgs = sqlx::query_scalar!(
            "SELECT id FROM organization WHERE parent_id = $1 ORDER BY id",
            organization
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let projects = sqlx::query_scalar!(
            "SELECT id FROM project WHERE organization_id = $1 ORDER BY id",
            organization
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok((orgs, projects))
    }
}
