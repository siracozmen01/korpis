// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's replicas (§2.2 of docs/06-storage.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{ReplicaRead, ReplicaWrite, StoreResult};
use korpis_core::workload::replica::Replica;
use korpis_core::workload::transfers::Flow;
use korpis_core::workload::volume::Volume;
use std::time::SystemTime;
use uuid::Uuid;

/// The advisory lock every choice of replicas is made under.
const REPLICA_LOCK: i64 = 0x6b6f_7270_6973_0004;

/// One stored replica.
struct ReplicaRow {
    volume_id: Uuid,
    node_id: Uuid,
    chosen_at: chrono::DateTime<chrono::Utc>,
    snapshot: Option<Uuid>,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
    sending: Option<Uuid>,
    sending_since: Option<chrono::DateTime<chrono::Utc>>,
    stale_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ReplicaRow {
    fn into_domain(self) -> Replica {
        Replica {
            volume: self.volume_id,
            node: self.node_id,
            chosen_at: to_time(self.chosen_at),
            held: self.snapshot.zip(self.received_at.map(to_time)),
            sending: self.sending.zip(self.sending_since.map(to_time)),
            stale_at: self.stale_at.map(to_time),
        }
    }
}

/// Fills in the replicas of each of `volumes`.
pub(crate) async fn with_replicas(pool: &sqlx::PgPool, volumes: &mut [Volume]) -> StoreResult<()> {
    if volumes.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = volumes.iter().map(|v| v.id).collect();
    let rows = sqlx::query_file_as!(ReplicaRow, "queries/replicas_of_volumes.sql", &ids)
        .fetch_all(pool)
        .await
        .map_err(from_sqlx)?;
    for r in rows {
        if let Some(v) = volumes.iter_mut().find(|v| v.id == r.volume_id) {
            v.replicas.push(r.into_domain());
        }
    }
    Ok(())
}

impl ReplicaRead for PgStore {
    async fn replicas_of(&self, volume: Uuid) -> StoreResult<Vec<Replica>> {
        let rows = sqlx::query_file_as!(ReplicaRow, "queries/replicas_of.sql", volume)
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(ReplicaRow::into_domain).collect())
    }

    async fn flows(&self) -> StoreResult<Vec<Flow>> {
        let rows = sqlx::query_file_as!(FlowRow, "queries/replica_flows.sql")
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(FlowRow::into_domain).collect())
    }
}

/// One replica with its volume's node and helper identifier.
struct FlowRow {
    volume_id: Uuid,
    node_id: Uuid,
    chosen_at: chrono::DateTime<chrono::Utc>,
    snapshot: Option<Uuid>,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
    sending: Option<Uuid>,
    sending_since: Option<chrono::DateTime<chrono::Utc>>,
    stale_at: Option<chrono::DateTime<chrono::Utc>>,
    from_node: Uuid,
    helper_id: String,
}

impl FlowRow {
    fn into_domain(self) -> Flow {
        let replica = ReplicaRow {
            volume_id: self.volume_id,
            node_id: self.node_id,
            chosen_at: self.chosen_at,
            snapshot: self.snapshot,
            received_at: self.received_at,
            sending: self.sending,
            sending_since: self.sending_since,
            stale_at: self.stale_at,
        };
        Flow { replica: replica.into_domain(), from: self.from_node, helper_id: self.helper_id }
    }
}

impl ReplicaWrite for PgTx {
    async fn lock_replicas(&mut self) -> StoreResult<()> {
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", REPLICA_LOCK)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn replicas_in(&mut self, volume: Uuid) -> StoreResult<Vec<Replica>> {
        let rows = sqlx::query_file_as!(ReplicaRow, "queries/replicas_of.sql", volume)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(ReplicaRow::into_domain).collect())
    }

    async fn choose_replica(
        &mut self,
        (volume, node): (Uuid, Uuid),
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO volume_replica (volume_id, node_id, chosen_at) VALUES ($1, $2, $3)
             ON CONFLICT (volume_id, node_id) DO NOTHING",
            volume,
            node,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn drop_replica(&mut self, volume: Uuid, node: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!(
            "DELETE FROM volume_replica WHERE volume_id = $1 AND node_id = $2",
            volume,
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn send_replica(
        &mut self,
        (volume, node): (Uuid, Uuid),
        snapshot: Uuid,
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE volume_replica SET sending = $3, sending_since = $4
              WHERE volume_id = $1 AND node_id = $2",
            volume,
            node,
            snapshot,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn replica_received(
        &mut self,
        (volume, node): (Uuid, Uuid),
        snapshot: Uuid,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE volume_replica
                SET snapshot = sending, received_at = $4, sending = NULL, sending_since = NULL,
                    stale_at = NULL
              WHERE volume_id = $1 AND node_id = $2 AND sending = $3",
            volume,
            node,
            snapshot,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn replica_failed(
        &mut self,
        (volume, node): (Uuid, Uuid),
        snapshot: Uuid,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE volume_replica SET sending = NULL, sending_since = NULL
              WHERE volume_id = $1 AND node_id = $2 AND sending = $3",
            volume,
            node,
            snapshot
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn mark_stale(
        &mut self,
        (volume, node): (Uuid, Uuid),
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE volume_replica SET stale_at = $3, sending = NULL, sending_since = NULL
              WHERE volume_id = $1 AND node_id = $2 AND stale_at IS NULL",
            volume,
            node,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
