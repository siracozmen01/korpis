// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3 of docs/07-networking.md, migration 0053).

use crate::errors::from_sqlx;
use crate::stored::from_time;
use crate::{PgStore, PgTx};
use korpis_app::ports::{EndpointRead, EndpointWrite, StoreError, StoreResult};
use korpis_core::node::advertised::PortRange;
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::spec::endpoints::{Exposure, Protocol};
use std::time::SystemTime;
use uuid::Uuid;

const PROTOCOLS: [(Protocol, &str); 5] = [
    (Protocol::Tcp, "tcp"),
    (Protocol::Udp, "udp"),
    (Protocol::Http, "http"),
    (Protocol::Https, "https"),
    (Protocol::Quic, "quic"),
];

pub(crate) const EXPOSURES: [(Exposure, &str); 7] = [
    (Exposure::NodePort, "node_port"),
    (Exposure::NodeBound, "node_bound"),
    (Exposure::Stable, "stable"),
    (Exposure::Ingress, "ingress"),
    (Exposure::DedicatedIp, "dedicated_ip"),
    (Exposure::OverlayOnly, "overlay_only"),
    (Exposure::None, "none"),
];

pub(crate) fn named<T: Copy + PartialEq>(table: &[(T, &'static str)], v: T) -> &'static str {
    table.iter().find(|(t, _)| *t == v).map_or("", |(_, n)| n)
}

pub(crate) fn parsed<T: Copy>(table: &[(T, &'static str)], what: &str, s: &str) -> StoreResult<T> {
    table
        .iter()
        .find(|(_, n)| *n == s)
        .map(|(t, _)| *t)
        .ok_or_else(|| StoreError::Corrupt(format!("endpoint {what} {s:?}")))
}

fn port(what: &str, n: i32) -> StoreResult<u16> {
    u16::try_from(n).map_err(|_| StoreError::Corrupt(format!("endpoint {what} {n}")))
}

struct EndpointRow {
    id: Uuid,
    workload_id: Uuid,
    name: String,
    protocol: String,
    container_port: i32,
    exposure: String,
    node_id: Uuid,
    port: Option<i32>,
}

impl EndpointRow {
    fn into_domain(self) -> StoreResult<Endpoint> {
        Ok(Endpoint {
            id: self.id,
            workload: self.workload_id,
            name: self.name,
            protocol: parsed(&PROTOCOLS, "protocol", &self.protocol)?,
            container_port: port("container port", self.container_port)?,
            exposure: parsed(&EXPOSURES, "exposure", &self.exposure)?,
            node: self.node_id,
            port: self.port.map(|p| port("port", p)).transpose()?,
        })
    }
}

fn domain(rows: Vec<EndpointRow>) -> StoreResult<Vec<Endpoint>> {
    rows.into_iter().map(EndpointRow::into_domain).collect()
}

impl EndpointRead for PgStore {
    async fn endpoints(&self, workload: Uuid) -> StoreResult<Vec<Endpoint>> {
        let rows = sqlx::query_as!(
            EndpointRow,
            "SELECT id, workload_id, name, protocol, container_port, exposure, node_id, port
               FROM endpoint WHERE workload_id = $1 ORDER BY name",
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        domain(rows)
    }

    async fn endpoint(&self, id: Uuid) -> StoreResult<Option<Endpoint>> {
        let rows = sqlx::query_as!(
            EndpointRow,
            "SELECT id, workload_id, name, protocol, container_port, exposure, node_id, port
               FROM endpoint WHERE id = $1",
            id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(domain(rows)?.into_iter().next())
    }

    async fn node_ports_held(&self, node: Uuid) -> StoreResult<Vec<u16>> {
        let held = sqlx::query_scalar!(
            r#"SELECT port AS "port!" FROM endpoint WHERE node_id = $1 AND port IS NOT NULL
                ORDER BY port"#,
            node
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        held.into_iter().map(|p| port("port", p)).collect()
    }
}

impl EndpointWrite for PgTx {
    async fn release_endpoints(
        &mut self,
        node: Uuid,
        workload: Uuid,
        keep: &[String],
    ) -> StoreResult<()> {
        sqlx::query!(
            "DELETE FROM endpoint e USING workload w
              WHERE e.workload_id = w.id AND e.node_id = $1 AND w.deleted_at IS NOT NULL",
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        // An endpoint keeps its identity when its workload moves, and gives back its port on
        // the node it left.
        sqlx::query!(
            "UPDATE endpoint SET node_id = $1, port = NULL WHERE workload_id = $2 AND node_id <> $1",
            node,
            workload
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "DELETE FROM endpoint WHERE workload_id = $1 AND NOT (name = ANY($2::text[]))",
            workload,
            keep
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn held_endpoints(&mut self, workload: Uuid) -> StoreResult<Vec<Endpoint>> {
        let rows = sqlx::query_as!(
            EndpointRow,
            "SELECT id, workload_id, name, protocol, container_port, exposure, node_id, port
               FROM endpoint WHERE workload_id = $1 ORDER BY name",
            workload
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        domain(rows)
    }

    async fn free_node_port(&mut self, node: Uuid, range: PortRange) -> StoreResult<Option<u16>> {
        let free = sqlx::query_scalar!(
            "SELECT p::integer AS port
               FROM generate_series($2::integer, $3::integer) AS p
              WHERE NOT EXISTS (SELECT 1 FROM endpoint WHERE node_id = $1 AND port = p)
              ORDER BY p LIMIT 1",
            node,
            i32::from(range.first()),
            i32::from(range.last())
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .flatten();
        free.map(|p| port("port", p)).transpose()
    }

    async fn put_endpoint(&mut self, e: &Endpoint, at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO endpoint (id, workload_id, name, protocol, container_port, exposure,
                                   node_id, port, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (workload_id, name) DO UPDATE
                SET protocol = EXCLUDED.protocol, container_port = EXCLUDED.container_port,
                    exposure = EXCLUDED.exposure, node_id = EXCLUDED.node_id,
                    port = EXCLUDED.port",
            e.id,
            e.workload,
            e.name,
            named(&PROTOCOLS, e.protocol),
            i32::from(e.container_port),
            named(&EXPOSURES, e.exposure),
            e.node,
            e.port.map(i32::from),
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_protocol_and_exposure_has_one_stored_name() {
        for (p, n) in PROTOCOLS {
            assert_eq!(parsed(&PROTOCOLS, "protocol", named(&PROTOCOLS, p)).ok(), Some(p), "{n}");
        }
        for (e, n) in EXPOSURES {
            assert_eq!(parsed(&EXPOSURES, "exposure", named(&EXPOSURES, e)).ok(), Some(e), "{n}");
        }
    }
}
