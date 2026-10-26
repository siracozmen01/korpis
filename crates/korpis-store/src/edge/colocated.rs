// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nodes each control plane replica shares its network with (migration 0135, §3.1 of
//! docs/07-networking.md), recorded with an end so that a replica that stops leaves none standing.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::StoreResult;
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

/// The nodes some replica's record says it shares a network with at `now`.
pub(super) async fn nodes(pool: &sqlx::PgPool, now: SystemTime) -> StoreResult<BTreeSet<Uuid>> {
    let rows = sqlx::query_scalar!(
        "SELECT DISTINCT node_id FROM node_colocated WHERE until > $1",
        from_time(now)
    )
    .fetch_all(pool)
    .await
    .map_err(from_sqlx)?;
    Ok(rows.into_iter().collect())
}

/// Records that `replica` shares its network with each of `nodes` until `until`, and with no
/// other.
pub(super) async fn record(
    tx: &mut PgTx,
    replica: Uuid,
    nodes: &[Uuid],
    until: SystemTime,
) -> StoreResult<()> {
    sqlx::query!(
        "DELETE FROM node_colocated WHERE replica = $1 AND NOT (node_id = ANY($2))",
        replica,
        nodes
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    sqlx::query!(
        "INSERT INTO node_colocated (node_id, replica, until)
         SELECT n, $1, $3 FROM unnest($2::uuid[]) AS n
             WHERE EXISTS (SELECT 1 FROM node WHERE id = n)
         ON CONFLICT (node_id, replica) DO UPDATE SET until = $3",
        replica,
        nodes,
        from_time(until)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
