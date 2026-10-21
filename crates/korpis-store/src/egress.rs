// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress budget's state (§5.1 of docs/05-scheduling.md, "The egress budget"): a month's
//! egress by workload and node, read from the metering records as an invoice sums them, what
//! each entry was reckoned to have spent, and what each workload of a spent entry is held to.

use crate::errors::from_sqlx;
use crate::quota::EntryRow;
use crate::stored::{count, from_json, from_time, to_count, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{EgressAt, EgressRead, EgressWrite, StoreError, StoreResult};
use korpis_core::tenancy::quota::{Exhaustion, QuotaEntry, Spent};
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

fn label_map(what: &str, v: Option<serde_json::Value>) -> StoreResult<BTreeMap<String, String>> {
    v.map_or_else(|| Ok(BTreeMap::new()), |v| from_json(what, v))
}

fn held(floor: Option<i64>, cut_off: bool) -> StoreResult<Exhaustion> {
    match (cut_off, floor) {
        (true, _) => Ok(Exhaustion::CutOff),
        (false, Some(bits)) => Ok(Exhaustion::Floor(count("egress floor", bits)?)),
        (false, None) => Err(StoreError::Corrupt("a workload held to no floor".into())),
    }
}

impl EgressRead for PgStore {
    async fn budgeted_quota_entries(&self) -> StoreResult<Vec<QuotaEntry>> {
        let rows = sqlx::query_as!(
            EntryRow,
            "SELECT id, owner_kind, owner_id, selector, nodes, reservations, limits, version, held,
                    egress_budget
               FROM quota_entry WHERE egress_budget IS NOT NULL ORDER BY id"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(EntryRow::into_domain).collect()
    }

    async fn egress_at(&self, from: SystemTime, to: SystemTime) -> StoreResult<Vec<EgressAt>> {
        let rows = sqlx::query_file!("queries/egress_at.sql", from_time(from), from_time(to))
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(EgressAt {
                    workload: r.workload_id,
                    project: r.project_id,
                    organization: r.organization_id,
                    labels: label_map("workload labels", Some(r.labels))?,
                    node: r.node_id,
                    node_labels: label_map("node labels", r.node_labels)?,
                    bytes: count("egress bytes", r.bytes)?,
                    placed: r.placed,
                })
            })
            .collect()
    }

    async fn workload_egress_all(&self) -> StoreResult<BTreeMap<Uuid, (SystemTime, Exhaustion)>> {
        let rows = sqlx::query!(
            "SELECT workload_id, month, floor_bits_per_sec, cut_off FROM workload_egress"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let held = held(r.floor_bits_per_sec, r.cut_off)?;
                Ok((r.workload_id, (to_time(r.month), held)))
            })
            .collect()
    }

    async fn workload_egress(
        &self,
        workload: Uuid,
        month: SystemTime,
    ) -> StoreResult<Option<Exhaustion>> {
        let row = sqlx::query!(
            "SELECT floor_bits_per_sec, cut_off FROM workload_egress
              WHERE workload_id = $1 AND month = $2",
            workload,
            from_time(month)
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| held(r.floor_bits_per_sec, r.cut_off)).transpose()
    }

    async fn quota_egress_spent(&self, entries: &[Uuid]) -> StoreResult<BTreeMap<Uuid, Spent>> {
        let rows = sqlx::query!(
            "SELECT entry_id, month, bytes, exhausted, reckoned_at FROM quota_egress
              WHERE entry_id = ANY($1)",
            entries
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let spent = Spent {
                    month: to_time(r.month),
                    bytes: count("egress spent", r.bytes)?,
                    exhausted: r.exhausted,
                    reckoned_at: to_time(r.reckoned_at),
                };
                Ok((r.entry_id, spent))
            })
            .collect()
    }
}

impl EgressWrite for PgTx {
    async fn put_quota_egress(&mut self, entry: Uuid, spent: &Spent) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO quota_egress (entry_id, month, bytes, exhausted, reckoned_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (entry_id) DO UPDATE SET month = $2, bytes = $3, exhausted = $4,
                    reckoned_at = $5",
            entry,
            from_time(spent.month),
            to_count("egress spent", spent.bytes)?,
            spent.exhausted,
            from_time(spent.reckoned_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn put_workload_egress(
        &mut self,
        workload: Uuid,
        month: SystemTime,
        held: Exhaustion,
    ) -> StoreResult<()> {
        let (floor, cut_off) = match held {
            Exhaustion::Floor(bits) => (Some(to_count("egress floor", bits)?), false),
            Exhaustion::CutOff => (None, true),
        };
        sqlx::query!(
            "INSERT INTO workload_egress (workload_id, month, floor_bits_per_sec, cut_off)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workload_id) DO UPDATE SET month = $2, floor_bits_per_sec = $3,
                    cut_off = $4",
            workload,
            from_time(month),
            floor,
            cut_off
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn clear_workload_egress(&mut self, workload: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM workload_egress WHERE workload_id = $1", workload)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }
}
