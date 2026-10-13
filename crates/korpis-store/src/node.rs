// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes (§3.4 of docs/01-model.md).

use crate::errors::from_sqlx;
use crate::stored::{
    bounds, bounds_doc, count, drivers, from_time, node_docs, storage_classes, taints, to_count,
    to_json, to_time,
};
use crate::tenancy::{labels, name};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{NodeRead, NodeWrite, StoreError, StoreResult};
use korpis_core::node::advertised::{Advertised, PortRange};
use korpis_core::node::{Connectivity, DriverInfo, Node, NodeAdmin, Reachability};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use serde_json::Value;
use std::net::IpAddr;
use uuid::Uuid;

pub(crate) struct NodeRow {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) organization_id: Uuid,
    pub(crate) labels: Value,
    pub(crate) taints: Value,
    pub(crate) capacity: Value,
    pub(crate) allocatable: Value,
    pub(crate) drivers: Value,
    pub(crate) storage_classes: Value,
    pub(crate) reachability: String,
    pub(crate) admin: String,
    pub(crate) connectivity: String,
    pub(crate) agent_version: Option<String>,
    pub(crate) last_seen: Option<DateTime<Utc>>,
    pub(crate) enrolled_at: DateTime<Utc>,
    pub(crate) version: i64,
    pub(crate) addresses: Vec<String>,
    pub(crate) first_port: Option<i32>,
    pub(crate) last_port: Option<i32>,
    pub(crate) probed_at: Option<DateTime<Utc>>,
}

/// What a node advertises, from its columns; none until it has reported a range.
fn advertised(
    addresses: &[String],
    first: Option<i32>,
    last: Option<i32>,
) -> StoreResult<Option<Advertised>> {
    let (Some(first), Some(last)) = (first, last) else { return Ok(None) };
    let port = |p: i32| u16::try_from(p).map_err(|_| corrupt("port", &p.to_string()));
    let ports = PortRange::new(port(first)?, port(last)?)
        .map_err(|e| StoreError::Corrupt(format!("node ports: {e}")))?;
    let addresses = addresses
        .iter()
        .map(|a| a.parse::<IpAddr>().map_err(|_| corrupt("address", a)))
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(Some(Advertised { addresses, ports }))
}

fn corrupt(what: &str, s: &str) -> StoreError {
    StoreError::Corrupt(format!("node {what} {s:?}"))
}

fn admin_str(a: NodeAdmin) -> &'static str {
    match a {
        NodeAdmin::Active => "active",
        NodeAdmin::Cordoned => "cordoned",
        NodeAdmin::Draining => "draining",
        NodeAdmin::Decommissioned => "decommissioned",
    }
}

fn connectivity_str(c: Connectivity) -> &'static str {
    match c {
        Connectivity::Registering => "registering",
        Connectivity::Connected => "connected",
        Connectivity::Unreachable => "unreachable",
        Connectivity::Unknown => "unknown",
    }
}

pub(crate) fn reachability_str(r: Reachability) -> &'static str {
    match r {
        Reachability::Public => "public",
        Reachability::EdgeOnly => "edge_only",
    }
}

impl NodeRow {
    pub(crate) fn into_domain(self) -> StoreResult<Node> {
        let admin = [
            NodeAdmin::Active,
            NodeAdmin::Cordoned,
            NodeAdmin::Draining,
            NodeAdmin::Decommissioned,
        ]
        .into_iter()
        .find(|a| admin_str(*a) == self.admin)
        .ok_or_else(|| corrupt("admin", &self.admin))?;
        let connectivity = [
            Connectivity::Registering,
            Connectivity::Connected,
            Connectivity::Unreachable,
            Connectivity::Unknown,
        ]
        .into_iter()
        .find(|c| connectivity_str(*c) == self.connectivity)
        .ok_or_else(|| corrupt("connectivity", &self.connectivity))?;
        let reachability = [Reachability::Public, Reachability::EdgeOnly]
            .into_iter()
            .find(|r| reachability_str(*r) == self.reachability)
            .ok_or_else(|| corrupt("reachability", &self.reachability))?;
        Ok(Node {
            id: self.id,
            name: name(self.name)?,
            organization: self.organization_id,
            labels: labels(self.labels)?,
            taints: taints(self.taints)?,
            capacity: bounds("node capacity", self.capacity)?,
            allocatable: bounds("node allocatable", self.allocatable)?,
            drivers: drivers(self.drivers)?,
            storage_classes: storage_classes(self.storage_classes)?,
            reachability,
            advertised: advertised(&self.addresses, self.first_port, self.last_port)?,
            probed_at: self.probed_at.map(to_time),
            admin,
            connectivity,
            agent_version: self.agent_version,
            last_seen: self.last_seen.map(to_time),
            enrolled_at: to_time(self.enrolled_at),
            version: count("node version", self.version)?,
        })
    }
}

impl NodeRead for PgStore {
    async fn node(&self, id: Uuid) -> StoreResult<Option<Node>> {
        let row = sqlx::query_file_as!(NodeRow, "queries/node_by_id.sql", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(NodeRow::into_domain).transpose()
    }

    async fn nodes(&self, organization: Uuid, page: Page) -> StoreResult<Vec<Node>> {
        let size = i64::from(page.size);
        let rows =
            sqlx::query_file_as!(NodeRow, "queries/nodes.sql", organization, page.after, size)
                .fetch_all(&self.pool)
                .await
                .map_err(from_sqlx)?;
        rows.into_iter().map(NodeRow::into_domain).collect()
    }

    async fn allocated(&self, node: Uuid) -> StoreResult<Bounds> {
        let rows = sqlx::query_file!("queries/node_allocated.sql", node)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let d = Dimension::parse(&r.dimension)
                    .ok_or_else(|| corrupt("reserved dimension", &r.dimension))?;
                Ok((d, count("node allocated", r.total)?))
            })
            .collect()
    }

    async fn serving_drivers(&self, owners: &[Uuid]) -> StoreResult<Vec<DriverInfo>> {
        let docs = sqlx::query_scalar!(
            "SELECT drivers FROM node
              WHERE organization_id = ANY($1) AND admin <> 'decommissioned' ORDER BY id",
            owners
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut all = Vec::new();
        for doc in docs {
            all.extend(drivers(doc)?);
        }
        Ok(all)
    }
}

impl NodeWrite for PgTx {
    async fn enroll_node(&mut self, n: &Node, public_key: &[u8]) -> StoreResult<()> {
        let (t, d, c) = node_docs(&n.taints, &n.drivers, &n.storage_classes);
        sqlx::query_file!(
            "queries/node_insert.sql",
            n.id,
            n.name.as_str(),
            n.organization,
            to_json("node labels", n.labels.as_map())?,
            to_json("node taints", &t)?,
            public_key,
            bounds_doc(&n.capacity),
            bounds_doc(&n.allocatable),
            to_json("node drivers", &d)?,
            to_json("node storage classes", &c)?,
            reachability_str(n.reachability),
            admin_str(n.admin),
            connectivity_str(n.connectivity),
            n.agent_version,
            n.last_seen.map(from_time),
            from_time(n.enrolled_at),
            to_count("node version", n.version)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn lock_node(&mut self, id: Uuid) -> StoreResult<Option<Node>> {
        let row = sqlx::query_file_as!(NodeRow, "queries/node_for_update.sql", id)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        row.map(NodeRow::into_domain).transpose()
    }

    async fn update_node_admin(&mut self, n: &Node, expected: u64) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE node SET admin = $2, version = version + 1 WHERE id = $1 AND version = $3",
            n.id,
            admin_str(n.admin),
            to_count("node version", expected)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
