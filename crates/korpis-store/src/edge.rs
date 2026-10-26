// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups and the stable addresses and ports of `stable` endpoints (migration 0132, §3.1 of
//! docs/07-networking.md). A group's members are rows of their own; a stable address and port is
//! one endpoint's at most, which its key holds, and keeps its row, resting, once released.

use crate::address::address;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{EdgeRead, EdgeWrite, StoreError, StoreResult};
use korpis_core::address::Taken;
use korpis_core::edge::{EdgeGroup, StablePort, TakenPorts};
use korpis_core::workload::endpoint::Transport;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

mod hold;
mod rows;

use rows::{GroupRow, PortRow, groups, millis, port_of, transport, transport_text};

impl EdgeRead for PgStore {
    async fn edge_group(&self, id: Uuid) -> StoreResult<Option<EdgeGroup>> {
        let row = sqlx::query_as!(
            GroupRow,
            r#"SELECT g.id, g.organization_id, g.name, g.first_port, g.last_port, g.probe_port,
                      g.probe_interval_ms, g.probe_misses, g.term_ms, g.created_at,
                      ARRAY(SELECT node_id FROM edge_member m WHERE m.group_id = g.id) AS members
                 FROM edge_group g WHERE g.id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(EdgeGroup::try_from).transpose()
    }

    async fn edge_groups_of(&self, organization: Uuid, page: Page) -> StoreResult<Vec<EdgeGroup>> {
        let rows = sqlx::query_as!(
            GroupRow,
            r#"SELECT g.id, g.organization_id, g.name, g.first_port, g.last_port, g.probe_port,
                      g.probe_interval_ms, g.probe_misses, g.term_ms, g.created_at,
                      ARRAY(SELECT node_id FROM edge_member m WHERE m.group_id = g.id) AS members
                 FROM edge_group g
                WHERE g.organization_id = $1 AND ($2::uuid IS NULL OR g.id > $2)
                ORDER BY g.id LIMIT $3"#,
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        groups(rows)
    }

    async fn edge_groups_all(&self) -> StoreResult<Vec<EdgeGroup>> {
        let rows = sqlx::query_as!(
            GroupRow,
            r#"SELECT g.id, g.organization_id, g.name, g.first_port, g.last_port, g.probe_port,
                      g.probe_interval_ms, g.probe_misses, g.term_ms, g.created_at,
                      ARRAY(SELECT node_id FROM edge_member m WHERE m.group_id = g.id) AS members
                 FROM edge_group g ORDER BY g.id"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        groups(rows)
    }

    async fn edge_group_named(&self, group: Uuid) -> StoreResult<bool> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM address_pool WHERE group_id = $1) AS "named!""#,
            group
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn stable_of(&self, workload: Uuid) -> StoreResult<Vec<StablePort>> {
        let rows = sqlx::query_as!(
            PortRow,
            r#"SELECT host(address) AS "address!", port, transport, pool_id, workload_id, endpoint
                 FROM stable_port WHERE workload_id = $1
                ORDER BY endpoint, family(address)"#,
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(port_of).collect()
    }

    async fn stable_all(&self) -> StoreResult<Vec<(Uuid, StablePort)>> {
        let rows = sqlx::query!(
            r#"SELECT p.group_id AS "group_id!", host(s.address) AS "address!", s.port,
                      s.transport, s.pool_id, s.workload_id, s.endpoint
                 FROM stable_port s JOIN address_pool p ON p.id = s.pool_id
                WHERE s.workload_id IS NOT NULL ORDER BY s.address, s.port"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let row = PortRow {
                    address: r.address,
                    port: r.port,
                    transport: r.transport,
                    pool_id: r.pool_id,
                    workload_id: r.workload_id,
                    endpoint: r.endpoint,
                };
                Ok((r.group_id, port_of(row)?))
            })
            .collect()
    }
}

impl PgTx {
    async fn put_members(&mut self, g: &EdgeGroup) -> StoreResult<()> {
        sqlx::query!("DELETE FROM edge_member WHERE group_id = $1", g.id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        sqlx::query!(
            "INSERT INTO edge_member (group_id, node_id) SELECT $1, unnest($2::uuid[])",
            g.id,
            &g.members
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}

impl EdgeWrite for PgTx {
    async fn insert_edge_group(&mut self, g: &EdgeGroup) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO edge_group (id, organization_id, name, first_port, last_port, probe_port,
                                     probe_interval_ms, probe_misses, term_ms, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            g.id,
            g.organization,
            g.name,
            i32::from(g.ports.0),
            i32::from(g.ports.1),
            i32::from(g.probe_port),
            millis(g.probe_interval),
            i32::try_from(g.probe_misses).unwrap_or(i32::MAX),
            millis(g.term),
            from_time(g.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        self.put_members(g).await
    }

    async fn update_edge_group(&mut self, g: &EdgeGroup) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE edge_group SET first_port = $2, last_port = $3, probe_port = $4,
                                   probe_interval_ms = $5, probe_misses = $6, term_ms = $7
              WHERE id = $1",
            g.id,
            i32::from(g.ports.0),
            i32::from(g.ports.1),
            i32::from(g.probe_port),
            millis(g.probe_interval),
            i32::try_from(g.probe_misses).unwrap_or(i32::MAX),
            millis(g.term)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if done.rows_affected() == 0 {
            return Ok(false);
        }
        self.put_members(g).await?;
        Ok(true)
    }

    async fn delete_edge_group(&mut self, id: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!("DELETE FROM edge_group WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(done.rows_affected() > 0)
    }

    async fn edge_groups_now(&mut self, organizations: &[Uuid]) -> StoreResult<Vec<EdgeGroup>> {
        let rows = sqlx::query_as!(
            GroupRow,
            r#"SELECT g.id, g.organization_id, g.name, g.first_port, g.last_port, g.probe_port,
                      g.probe_interval_ms, g.probe_misses, g.term_ms, g.created_at,
                      ARRAY(SELECT node_id FROM edge_member m WHERE m.group_id = g.id) AS members
                 FROM edge_group g WHERE g.organization_id = ANY($1) ORDER BY g.id"#,
            organizations
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        groups(rows)
    }

    async fn stable_taken(&mut self, pool: Uuid) -> StoreResult<TakenPorts> {
        let rows = sqlx::query!(
            r#"SELECT host(address) AS "address!", port, transport, workload_id, released_at
                 FROM stable_port WHERE pool_id = $1"#,
            pool
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let corrupt =
                    || StoreError::Corrupt(format!("stable port {}:{}", r.address, r.port));
                let taken = match (r.workload_id, r.released_at) {
                    (Some(w), _) => Taken::Held(w),
                    (None, Some(at)) => Taken::Released(to_time(at)),
                    (None, None) => return Err(corrupt()),
                };
                let port = u16::try_from(r.port).map_err(|_| corrupt())?;
                let t = transport(&r.transport).ok_or_else(corrupt)?;
                Ok(((address(&r.address)?, port, t), taken))
            })
            .collect()
    }

    async fn stable_now(&mut self, workload: Uuid) -> StoreResult<Vec<StablePort>> {
        let rows = sqlx::query_as!(
            PortRow,
            r#"SELECT host(address) AS "address!", port, transport, pool_id, workload_id, endpoint
                 FROM stable_port WHERE workload_id = $1
                ORDER BY endpoint, family(address)"#,
            workload
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(port_of).collect()
    }

    async fn hold_stable(&mut self, held: &StablePort, at: SystemTime) -> StoreResult<()> {
        let done = sqlx::query!(
            "INSERT INTO stable_port (address, port, transport, pool_id, workload_id, endpoint,
                                      held_at, released_at)
             VALUES ($1::text::inet, $2, $7, $3, $4, $5, $6, NULL)
             ON CONFLICT (address, port, transport) DO UPDATE
                SET pool_id = $3, workload_id = $4, endpoint = $5, held_at = $6, released_at = NULL
              WHERE stable_port.workload_id IS NULL",
            held.address.to_string(),
            i32::from(held.port),
            held.pool,
            held.workload,
            held.endpoint,
            from_time(at),
            transport_text(held.transport)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        // Another endpoint holds it: placement chooses under its lock, so this is a fault.
        if done.rows_affected() == 0 {
            return Err(StoreError::Duplicate(korpis_app::ports::Unique::Other));
        }
        Ok(())
    }

    async fn release_stable(
        &mut self,
        workload: Uuid,
        keep: &[String],
        at: SystemTime,
    ) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE stable_port SET workload_id = NULL, endpoint = NULL, released_at = $3
              WHERE workload_id = $1 AND NOT (endpoint = ANY($2))",
            workload,
            keep,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }

    async fn release_stable_port(
        &mut self,
        (address, port, t): (IpAddr, u16, Transport),
        when: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE stable_port SET workload_id = NULL, endpoint = NULL, released_at = $3
              WHERE address = $1::text::inet AND port = $2 AND transport = $4
                AND workload_id IS NOT NULL",
            address.to_string(),
            i32::from(port),
            from_time(when),
            transport_text(t)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn stable_held_in(&mut self, pool: Uuid) -> StoreResult<Option<IpAddr>> {
        let row = sqlx::query_scalar!(
            r#"SELECT host(address) AS "address!" FROM stable_port
                WHERE pool_id = $1 AND workload_id IS NOT NULL LIMIT 1"#,
            pool
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.as_deref().map(address).transpose()
    }
}
