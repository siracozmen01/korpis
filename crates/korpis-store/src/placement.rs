// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placements (§3.4 of docs/01-model.md, §2.3 of docs/05-scheduling.md).

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::count;
use korpis_app::ports::{PlacementWrite, StoreError, StoreResult};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The advisory lock every bind and every cordon holds: one key, for the installation.
const PLACEMENT_LOCK: i64 = 0x6b6f_7270_6973_0001;

impl PlacementWrite for PgTx {
    async fn lock_placement(&mut self) -> StoreResult<()> {
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", PLACEMENT_LOCK)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn allocations(&mut self, nodes: &[Uuid]) -> StoreResult<BTreeMap<Uuid, Bounds>> {
        let rows = sqlx::query_file!("queries/node_allocations.sql", nodes)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let mut out: BTreeMap<Uuid, Bounds> = BTreeMap::new();
        for r in rows {
            let d = Dimension::parse(&r.dimension).ok_or_else(|| {
                StoreError::Corrupt(format!("placement reserved: dimension {:?}", r.dimension))
            })?;
            out.entry(r.node_id).or_default().insert(d, count("node allocated", r.total)?);
        }
        Ok(out)
    }
}
