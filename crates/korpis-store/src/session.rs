// SPDX-License-Identifier: AGPL-3.0-or-later

//! What agent sessions learn: node facts, Hello nonces, and whether a node is heard.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{bounds_doc, cores_doc, devices_doc, from_time, node_docs, to_json};
use korpis_app::ports::{NodeFacts, SessionWrite, StoreResult};
use korpis_core::node::Reachability;
use korpis_core::node::advertised::Advertised;
use std::time::SystemTime;
use uuid::Uuid;

impl SessionWrite for PgTx {
    async fn remember_nonce(
        &mut self,
        node: Uuid,
        nonce: &[u8],
        at: SystemTime,
        forget_before: SystemTime,
    ) -> StoreResult<bool> {
        sqlx::query!(
            "DELETE FROM node_auth_nonce WHERE node_id = $1 AND seen_at < $2",
            node,
            from_time(forget_before)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let inserted = sqlx::query!(
            "INSERT INTO node_auth_nonce (node_id, nonce, seen_at) VALUES ($1, $2, $3)
             ON CONFLICT (node_id, nonce) DO NOTHING",
            node,
            nonce,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn record_node_facts(
        &mut self,
        node: Uuid,
        f: &NodeFacts,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let classes = f.storage_classes.as_deref().unwrap_or_default();
        let (_, drivers, classes) = node_docs(&[], &f.drivers, classes);
        let classes = f.storage_classes.as_ref().map(|_| classes);
        let release = f.agent.as_ref().map(|a| a.release.clone());
        let protocol = f.agent.as_ref().map(|a| a.protocol.clone());
        let addresses: Option<Vec<String>> =
            f.advertised.as_ref().map(|a| a.addresses.iter().map(ToString::to_string).collect());
        let first = f.advertised.as_ref().map(|a| i32::from(a.ports.first()));
        let last = f.advertised.as_ref().map(|a| i32::from(a.ports.last()));
        let ingress = f.advertised.as_ref().map(|a| a.ingress);
        let (devices, unoffered) = devices_doc(&f.devices, &f.unoffered_devices)?;
        let done = sqlx::query_file!(
            "queries/node_facts.sql",
            node,
            bounds_doc(&f.capacity),
            bounds_doc(&f.allocatable),
            to_json("node drivers", &drivers)?,
            classes.map(|c| to_json("node storage classes", &c)).transpose()?,
            release,
            protocol,
            from_time(at),
            addresses.as_deref(),
            first,
            last,
            ingress,
            devices,
            unoffered,
            cores_doc(&f.cores)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_reachability(
        &mut self,
        node: Uuid,
        measured: &Advertised,
        reachability: Reachability,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let addresses: Vec<String> = measured.addresses.iter().map(ToString::to_string).collect();
        let done = sqlx::query!(
            "UPDATE node SET reachability = $2, probed_at = $3
              WHERE id = $1 AND addresses = $4
                AND first_port = $5 AND last_port = $6",
            node,
            crate::node::reachability_str(reachability),
            from_time(at),
            &addresses,
            i32::from(measured.ports.first()),
            i32::from(measured.ports.last())
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_content(
        &mut self,
        node: Uuid,
        present: &[String],
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!("DELETE FROM node_content WHERE node_id = $1", node)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        sqlx::query!(
            "INSERT INTO node_content (node_id, digest)
             SELECT $1, d FROM unnest($2::text[]) AS d ON CONFLICT DO NOTHING",
            node,
            present
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!("UPDATE node SET content_reported_at = $2 WHERE id = $1", node, from_time(at))
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn mark_silent_nodes(&mut self, before: SystemTime, at: SystemTime) -> StoreResult<u64> {
        // A data-modifying CTE runs whether or not the query reads it.
        let marked = sqlx::query_scalar!(
            r#"WITH silenced AS (
                   UPDATE node SET connectivity = 'unreachable'
                    WHERE connectivity = 'connected' AND last_seen < $1
                   RETURNING id),
               lost AS (
                   INSERT INTO node_loss (node_id, at, workloads)
                   SELECT s.id, $2, count(*)
                     FROM silenced s JOIN placement p ON p.node_id = s.id
                    GROUP BY s.id
                   ON CONFLICT DO NOTHING
                   RETURNING node_id)
               SELECT count(*) AS "marked!" FROM silenced"#,
            from_time(before),
            from_time(at)
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(u64::try_from(marked).unwrap_or(0))
    }

    async fn forget_connections(&mut self) -> StoreResult<u64> {
        let done = sqlx::query!(
            "UPDATE node SET connectivity = 'unknown' WHERE connectivity = 'connected'"
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
