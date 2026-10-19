// SPDX-License-Identifier: AGPL-3.0-or-later

//! The quota entries that name the nodes they count, and where each placement was counted by
//! them (§5.1 of docs/05-scheduling.md, "Where a workload runs"). Read and written under the
//! placement lock, which every change to them holds.

use super::EntryRow;
use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{held, held_doc};
use crate::tenancy::labels;
use korpis_app::ports::{Counted, Placed, PlacedHolding, PlacedQuotaWrite, StoreResult};
use korpis_core::tenancy::quota::{Held, QuotaEntry, QuotaOwner};
use uuid::Uuid;

impl PlacedQuotaWrite for PgTx {
    async fn placed_quota(
        &mut self,
        project: Uuid,
        organizations: &[Uuid],
    ) -> StoreResult<Vec<QuotaEntry>> {
        let rows =
            sqlx::query_file_as!(EntryRow, "queries/quota_placed.sql", project, organizations)
                .fetch_all(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        rows.into_iter().map(EntryRow::into_domain).collect()
    }

    async fn placement_held(&mut self, workload: Uuid) -> StoreResult<Option<Placed>> {
        let row = sqlx::query!(
            "SELECT node_id, held, counted_by FROM placement_held WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(Placed {
                node: r.node_id,
                counted: Counted { held: held(r.held)?, by: r.counted_by },
            })
        })
        .transpose()
    }

    async fn put_placement_held(
        &mut self,
        workload: Uuid,
        placed: Option<&Placed>,
    ) -> StoreResult<()> {
        let Some(p) = placed else {
            sqlx::query!("DELETE FROM placement_held WHERE workload_id = $1", workload)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
            return Ok(());
        };
        sqlx::query!(
            "INSERT INTO placement_held (workload_id, node_id, held, counted_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workload_id) DO UPDATE SET node_id = $2, held = $3, counted_by = $4",
            workload,
            p.node,
            held_doc(&p.counted.held)?,
            &p.counted.by
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn placed_holdings(&mut self, owner: QuotaOwner) -> StoreResult<Vec<PlacedHolding>> {
        let (project, organization) = match owner {
            QuotaOwner::Project(p) => (Some(p), None),
            QuotaOwner::Organization(o) => (None, Some(o)),
        };
        let rows = sqlx::query_file!("queries/quota_placed_holdings.sql", project, organization)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(PlacedHolding {
                    workload: r.id,
                    labels: labels(r.labels)?.as_map().clone(),
                    node_labels: labels(r.node_labels)?.as_map().clone(),
                    placed: Placed {
                        node: r.node,
                        counted: Counted { held: held(r.held)?, by: r.counted_by },
                    },
                })
            })
            .collect()
    }

    async fn count_placed_in(&mut self, entry: Uuid, workloads: &[Uuid]) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE placement_held SET counted_by = array_append(counted_by, $1)
              WHERE workload_id = ANY($2) AND NOT ($1 = ANY(counted_by))",
            entry,
            workloads
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn placed_counted_by(&mut self, entry: Uuid) -> StoreResult<Vec<Held>> {
        let rows = sqlx::query_scalar!(
            "SELECT held FROM placement_held WHERE $1 = ANY(counted_by) ORDER BY workload_id",
            entry
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(held).collect()
    }
}
