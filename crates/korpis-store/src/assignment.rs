// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignments, kept with their generation for good, and what they are computed from.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_count};
use crate::{PgStore, PgTx};
use korpis_app::ports::{AssignmentRead, AssignmentWrite, Sealed, StoreResult, StoredAssignment};
use std::time::SystemTime;
use uuid::Uuid;

/// The namespace of the per-node advisory locks that serialize computing a node's set.
const ASSIGNMENT_LOCKS: i32 = 0x6b6f_7271;

impl AssignmentRead for PgStore {
    async fn placed_on(&self, node: Uuid) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT workload_id FROM placement WHERE node_id = $1 ORDER BY workload_id",
            node
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn nodes_of(&self, workload: Uuid) -> StoreResult<Vec<Uuid>> {
        let rows = sqlx::query_scalar!(
            r#"SELECT node_id AS "node!" FROM placement WHERE workload_id = $1
               UNION
               SELECT node_id FROM assignment WHERE workload_id = $1 AND active"#,
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows)
    }

    async fn lease_epoch(&self, workload: Uuid, node: Uuid) -> StoreResult<Option<u64>> {
        let epoch = sqlx::query_scalar!(
            "SELECT epoch FROM lease WHERE workload_id = $1 AND node_id = $2
               AND released_at IS NULL",
            workload,
            node
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        epoch.map(|e| count("lease epoch", e)).transpose()
    }
}

impl AssignmentWrite for PgTx {
    async fn lock_assignments(&mut self, node: Uuid) -> StoreResult<()> {
        // The two-key form, in a namespace of its own: the first key is not the first half of
        // the placement lock, so no node's key can collide with it.
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2::text))",
            ASSIGNMENT_LOCKS,
            node.to_string()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn stored_assignments(&mut self, node: Uuid) -> StoreResult<Vec<StoredAssignment>> {
        let rows = sqlx::query!(
            "SELECT workload_id, generation, content_digest, body, signature, key_id, active
               FROM assignment WHERE node_id = $1 ORDER BY workload_id",
            node
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                Ok(StoredAssignment {
                    workload: r.workload_id,
                    generation: count("assignment generation", r.generation)?,
                    content_digest: r.content_digest,
                    sealed: Sealed { body: r.body, signature: r.signature, key_id: r.key_id },
                    active: r.active,
                })
            })
            .collect()
    }

    async fn put_assignment(&mut self, node: Uuid, a: &StoredAssignment) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO assignment (workload_id, node_id, generation, body, signature, key_id,
                                     content_digest, active, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, true, now())
             ON CONFLICT (workload_id, node_id) DO UPDATE SET
                 generation = EXCLUDED.generation, body = EXCLUDED.body,
                 signature = EXCLUDED.signature, key_id = EXCLUDED.key_id,
                 content_digest = EXCLUDED.content_digest, active = true, updated_at = now()",
            a.workload,
            node,
            to_count("assignment generation", a.generation)?,
            a.sealed.body,
            a.sealed.signature,
            a.sealed.key_id,
            a.content_digest
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn deactivate_assignment(&mut self, node: Uuid, workload: Uuid) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE assignment SET active = false, updated_at = now()
              WHERE workload_id = $1 AND node_id = $2",
            workload,
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn record_ack(&mut self, node: Uuid, digest: &str, at: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE node SET acked_set_digest = $2, acked_at = $3 WHERE id = $1",
            node,
            digest,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
