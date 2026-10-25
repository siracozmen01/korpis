// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh's candidates (§9.1 of docs/07-networking.md): every node not decommissioned that
//! reports a mesh interface, with when it enrolled, the addresses it advertises, the pool it was
//! given, and its organization and each one above it, in one statement so they agree.

use crate::PgStore;
use crate::errors::from_sqlx;
use crate::stored::{overlay, to_time};
use korpis_app::ports::StoreResult;
use korpis_core::id::{Id, IdKind};
use korpis_core::node::mesh::Candidate;
use korpis_core::node::pool::unique_local;

impl PgStore {
    pub(crate) async fn candidates(&self) -> StoreResult<Vec<Candidate>> {
        let rows = sqlx::query!(
            r#"WITH RECURSIVE line AS (
                   SELECT n.id AS node, n.organization_id AS org, 0 AS depth
                     FROM node n WHERE n.admin <> 'decommissioned' AND n.overlay IS NOT NULL
                   UNION ALL
                   SELECT line.node, o.parent_id, line.depth + 1
                     FROM line JOIN organization o ON o.id = line.org
                    WHERE o.parent_id IS NOT NULL
               )
               SELECT n.id AS "id!", n.enrolled_at AS "enrolled_at!", n.addresses AS "addresses!",
                      n.overlay AS "overlay!",
                      p.ipv4::text AS "given?",
                      (SELECT array_agg(l.org ORDER BY l.depth) FROM line l WHERE l.node = n.id)
                          AS "lineage!: Vec<uuid::Uuid>"
                 FROM node n LEFT JOIN node_pool p ON p.node_id = n.id
                WHERE n.admin <> 'decommissioned' AND n.overlay IS NOT NULL"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(Candidate {
                    node: r.id,
                    enrolled_at: to_time(r.enrolled_at),
                    addresses: r.addresses.iter().filter_map(|a| a.parse().ok()).collect(),
                    facts: overlay(r.overlay)?,
                    given: r.given.and_then(|g| g.parse().ok()),
                    derived_ipv6: unique_local(&Id::from_uuid(IdKind::Node, r.id).to_string()),
                    lineage: r.lineage,
                })
            })
            .collect()
    }
}
