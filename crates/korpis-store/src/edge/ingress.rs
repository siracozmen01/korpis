// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ingress` through edge groups (migration 0136, §3.1 of docs/07-networking.md): each group's
//! ingress address of each family, the workloads whose `ingress` endpoints a group serves, and
//! the challenges each node's proxy holds of its groups' names.

use crate::address::address;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use crate::{PgStore, PgTx};
use korpis_app::ports::{ChallengesHeld, EdgeIngressRead, EdgeIngressWrite, StoreResult};
use korpis_core::edge::IngressAddress;
use std::time::SystemTime;
use uuid::Uuid;

struct Row {
    address: String,
    group_id: Uuid,
    pool_id: Uuid,
}

fn addresses(rows: Vec<Row>) -> StoreResult<Vec<IngressAddress>> {
    rows.into_iter()
        .map(|r| {
            Ok(IngressAddress { address: address(&r.address)?, group: r.group_id, pool: r.pool_id })
        })
        .collect()
}

impl EdgeIngressRead for PgStore {
    async fn ingress_addresses_all(&self) -> StoreResult<Vec<IngressAddress>> {
        let rows = sqlx::query_as!(
            Row,
            r#"SELECT host(address) AS "address!", group_id, pool_id FROM edge_ingress
                ORDER BY group_id, family(address), address"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        addresses(rows)
    }

    async fn ingress_addresses_of(&self, group: Uuid) -> StoreResult<Vec<IngressAddress>> {
        let rows = sqlx::query_as!(
            Row,
            r#"SELECT host(address) AS "address!", group_id, pool_id FROM edge_ingress
                WHERE group_id = $1 ORDER BY family(address), address"#,
            group
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        addresses(rows)
    }

    async fn ingress_served_all(&self) -> StoreResult<Vec<(Uuid, Uuid)>> {
        let rows =
            sqlx::query!("SELECT workload_id, group_id FROM ingress_served ORDER BY workload_id")
                .fetch_all(&self.pool)
                .await
                .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.workload_id, r.group_id)).collect())
    }

    async fn ingress_served_of(&self, workload: Uuid) -> StoreResult<Option<Uuid>> {
        sqlx::query_scalar!("SELECT group_id FROM ingress_served WHERE workload_id = $1", workload)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn proxied_by(&self, node: Uuid) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT s.workload_id FROM ingress_served s
               JOIN edge_member m ON m.group_id = s.group_id
              WHERE m.node_id = $1 ORDER BY s.workload_id",
            node
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn edge_challenges_held(&self, node: Uuid) -> StoreResult<ChallengesHeld> {
        let row = sqlx::query!(
            "SELECT tokens, digests FROM edge_challenges_held WHERE node_id = $1",
            node
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| ChallengesHeld { tokens: r.tokens, digests: r.digests }).unwrap_or_default())
    }
}

impl EdgeIngressWrite for PgTx {
    async fn ingress_addresses_now(&mut self, group: Uuid) -> StoreResult<Vec<IngressAddress>> {
        let rows = sqlx::query_as!(
            Row,
            r#"SELECT host(address) AS "address!", group_id, pool_id FROM edge_ingress
                WHERE group_id = $1 ORDER BY family(address), address"#,
            group
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        addresses(rows)
    }

    async fn hold_ingress_address(
        &mut self,
        a: &IngressAddress,
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO edge_ingress (address, group_id, pool_id, held_at)
             VALUES ($1::text::inet, $2, $3, $4)",
            a.address.to_string(),
            a.group,
            a.pool,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn serve_ingress(
        &mut self,
        workload: Uuid,
        group: Option<Uuid>,
        at: SystemTime,
    ) -> StoreResult<()> {
        let Some(group) = group else {
            sqlx::query!("DELETE FROM ingress_served WHERE workload_id = $1", workload)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
            return Ok(());
        };
        sqlx::query!(
            "INSERT INTO ingress_served (workload_id, group_id, held_at) VALUES ($1, $2, $3)
             ON CONFLICT (workload_id) DO UPDATE SET group_id = $2, held_at = $3
              WHERE ingress_served.group_id <> $2",
            workload,
            group,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn record_edge_challenges(
        &mut self,
        node: Uuid,
        held: &ChallengesHeld,
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO edge_challenges_held (node_id, tokens, digests, held_at)
             SELECT $1::uuid, $2::text[], $3::bytea[], $4::timestamptz
              WHERE EXISTS (SELECT 1 FROM node WHERE id = $1)
             ON CONFLICT (node_id) DO UPDATE SET tokens = $2, digests = $3, held_at = $4",
            node,
            held.tokens.as_slice(),
            held.digests.as_slice(),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
